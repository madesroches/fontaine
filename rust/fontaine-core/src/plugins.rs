//! Plugins and marketplaces: Claude Code's extension bundles installed from git-hosted catalogs.
//!
//! A plugin is a directory in Claude Code's layout (`.claude-plugin/plugin.json`, `skills/`,
//! `agents/`, `output-styles/`, `commands/`, `hooks/hooks.json`, `.mcp.json`) read at the same
//! paths so existing plugins port by copying. A marketplace is a git repository or local
//! directory carrying `.claude-plugin/marketplace.json`. Installs are user-scope only, pinned by
//! commit, and are the explicit action that approves a plugin's executables; a plugin's content
//! can only ever tighten permissions, never loosen them.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::{Hook, McpServer, Origin, Slot};
use crate::permission_rules::{RuleSource, SourcedRule, parse_rule};

pub const REGISTRY_FILE: &str = "plugins.toml";
pub const PLUGINS_DIR: &str = "plugins";
pub const MARKETPLACES_DIR: &str = "marketplaces";
pub const INSTALLED_DIR: &str = "installed";
pub const MANIFEST_FILE: &str = ".claude-plugin/plugin.json";
pub const MARKETPLACE_FILE: &str = ".claude-plugin/marketplace.json";
pub const HOOKS_FILE: &str = "hooks/hooks.json";
pub const MCP_FILE: &str = ".mcp.json";

/// A plugin directory ready for the extension loader, with its components resolved to files.
#[derive(Clone, Debug, Default)]
pub struct PluginDir {
    pub name: String,
    pub root: PathBuf,
    /// `skills/<name>/SKILL.md` files; naming segments before the base name are empty (skills
    /// are not walked recursively).
    pub skills: Vec<PluginFile>,
    /// `commands/**/*.md`, loaded as user-invocable skills.
    pub commands: Vec<PluginFile>,
    /// `agents/**/*.md`.
    pub agents: Vec<PluginFile>,
    /// `output-styles/*.md`.
    pub styles: Vec<PluginFile>,
    /// The plugin's own MCP servers, bare name to composed `plugin_<plugin>_<name>` name, so a
    /// bare `mcp__<name>__*` reference in a file is rewritten before validation.
    pub mcp_names: BTreeMap<String, String>,
}

/// One component file, with the directory segments that become part of its namespaced name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginFile {
    pub path: PathBuf,
    pub segments: Vec<String>,
}

/// The inputs that vary per session: the primary root for `${CLAUDE_PROJECT_DIR}` and the
/// process environment for `${VAR}` expansion in hook commands and `.mcp.json` values.
pub struct PluginContext<'a> {
    pub project_root: &'a Path,
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
}

/// A file that cannot be loaded, or a catalog/registry entry that cannot be used. Startup stops
/// with exit code 2, as extension load errors do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginError {
    pub plugin: Option<String>,
    pub file: PathBuf,
    pub message: String,
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.plugin {
            Some(plugin) => write!(
                f,
                "plugin `{plugin}`: {}: {}",
                self.file.display(),
                self.message
            ),
            None => write!(f, "{}: {}", self.file.display(), self.message),
        }
    }
}

impl std::error::Error for PluginError {}

impl PluginError {
    pub fn exit_code(&self) -> u8 {
        crate::config::CONFIG_EXIT_CODE
    }

    fn in_plugin(plugin: &str, file: PathBuf, message: impl Into<String>) -> PluginError {
        PluginError {
            plugin: Some(plugin.to_string()),
            file,
            message: message.into(),
        }
    }

    fn in_file(file: PathBuf, message: impl Into<String>) -> PluginError {
        PluginError {
            plugin: None,
            file,
            message: message.into(),
        }
    }
}

/// The hooks one plugin (or all plugins together) bring, kept per event so a hook runs only
/// at the event its entry names.
#[derive(Clone, Debug, Default)]
pub struct PluginHooks {
    pub pre_tool_use: Vec<Hook>,
    pub post_tool_use: Vec<Hook>,
    pub permission_denied: Vec<Hook>,
}

impl PluginHooks {
    pub fn len(&self) -> usize {
        self.pre_tool_use.len() + self.post_tool_use.len() + self.permission_denied.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What one installed, validated plugin contributes to a session.
#[derive(Clone, Debug, Default)]
pub struct PluginBuild {
    pub dir: PluginDir,
    pub hooks: PluginHooks,
    /// Every `${VAR}` the plugin's hook commands and `.mcp.json` values reference from the
    /// process environment, for the install summary: a variable's value can reach a plugin's
    /// server or hook, so the approval should name the variables.
    pub env_vars: BTreeSet<String>,
    pub servers: BTreeMap<String, McpServer>,
    pub deny: Vec<SourcedRule>,
    pub ask: Vec<SourcedRule>,
    pub hard_deny: Vec<String>,
    pub soft_deny: Vec<String>,
    pub warnings: Vec<String>,
    pub summary: PluginSummary,
}

/// The counts an install or update prints before touching the registry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PluginSummary {
    pub skills: usize,
    pub agents: usize,
    pub styles: usize,
    pub hooks: usize,
    pub servers: usize,
    pub deny: usize,
    pub ask: usize,
    pub hard_deny: usize,
    pub soft_deny: usize,
}

/// Everything enabled, installed plugins bring to a session.
#[derive(Clone, Debug, Default)]
pub struct PluginContributions {
    pub dirs: Vec<PluginDir>,
    pub hooks: PluginHooks,
    pub servers: BTreeMap<String, McpServer>,
    pub deny: Vec<SourcedRule>,
    pub ask: Vec<SourcedRule>,
    pub hard_deny: Vec<String>,
    pub soft_deny: Vec<String>,
    pub warnings: Vec<String>,
}

/// `plugins.toml`: the user-scope registry of marketplaces and installed plugins.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    #[serde(default)]
    pub marketplace: Vec<MarketplaceEntry>,
    #[serde(default)]
    pub plugin: Vec<InstalledPlugin>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketplaceEntry {
    pub name: String,
    /// A git URL or a local directory path.
    pub url: String,
    /// Pinned catalog commit; a content hash for a local directory.
    pub pin: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledPlugin {
    pub name: String,
    pub marketplace: String,
    /// One line naming where the plugin came from, kept for display; updates re-read the entry
    /// from the pinned catalog.
    pub source: String,
    /// Pinned install commit; a path source shares the marketplace's pin.
    pub pin: String,
    pub enabled: bool,
}

/// A parsed marketplace catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    pub name: String,
    pub plugins: Vec<CatalogPlugin>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogPlugin {
    pub name: String,
    pub source: CatalogSource,
    pub description: Option<String>,
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogSource {
    /// A relative path inside the marketplace repository.
    Path(String),
    Github {
        repo: String,
        r#ref: Option<String>,
        sha: Option<String>,
    },
    Url {
        url: String,
        r#ref: Option<String>,
        sha: Option<String>,
    },
}

impl CatalogSource {
    /// One line for the registry's `source` column.
    pub fn display(&self) -> String {
        match self {
            CatalogSource::Path(path) => path.clone(),
            CatalogSource::Github { repo, .. } => format!("github:{repo}"),
            CatalogSource::Url { url, .. } => format!("url:{url}"),
        }
    }
}

pub fn registry_path(config_dir: &Path) -> PathBuf {
    config_dir.join(REGISTRY_FILE)
}

pub fn marketplace_dir(config_dir: &Path, marketplace: &str) -> PathBuf {
    config_dir
        .join(PLUGINS_DIR)
        .join(MARKETPLACES_DIR)
        .join(marketplace)
}

pub fn checkout_dir(config_dir: &Path, marketplace: &str, plugin: &str) -> PathBuf {
    config_dir
        .join(PLUGINS_DIR)
        .join(INSTALLED_DIR)
        .join(marketplace)
        .join(plugin)
}

/// The short form of a pin for display; safe on a hand-edited, non-ASCII pin.
pub fn short_pin(pin: &str) -> &str {
    let mut end = pin.len().min(7);
    while !pin.is_char_boundary(end) {
        end -= 1;
    }
    &pin[..end]
}

/// Reads the registry without taking its lock; a caller that mutates it must hold
/// [`crate::file_lock::lock`] on [`registry_path`] around the read-modify-write. A missing file
/// is an empty registry.
pub fn read_registry(config_dir: &Path) -> Result<Registry, PluginError> {
    let path = registry_path(config_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Registry::default()),
        Err(e) => {
            return Err(PluginError::in_file(
                path,
                format!("cannot read it: {}", fontaine_tools::error_chain::chain(&e)),
            ));
        }
    };
    let registry: Registry = toml::from_str(&text).map_err(|e: toml::de::Error| {
        PluginError::in_file(
            path.clone(),
            crate::config::bare_toml_message(&text, e.span(), e.message()),
        )
    })?;
    registry_names_valid(&registry).map_err(|message| PluginError::in_file(path, message))?;
    Ok(registry)
}

/// Writes the registry atomically, owner-only.
pub fn write_registry(config_dir: &Path, registry: &Registry) -> Result<(), PluginError> {
    let path = registry_path(config_dir);
    let text = toml::to_string_pretty(registry).map_err(|e| {
        PluginError::in_file(path.clone(), format!("cannot encode the registry: {e}"))
    })?;
    crate::owner_only::write_atomic(&path, text.as_bytes()).map_err(|e| {
        PluginError::in_file(
            path,
            format!(
                "cannot write it: {}",
                fontaine_tools::error_chain::chain(&e)
            ),
        )
    })
}

/// Schema check for `plugins.toml` text, used by the docs check.
pub fn validate_plugins_toml(text: &str) -> Result<(), String> {
    let registry: Registry = toml::from_str(text).map_err(|e| e.to_string())?;
    registry_names_valid(&registry)
}

/// Whether every registry name is a valid path component and composed MCP name. A corrupt or
/// hand-edited registry must not steer a checkout path outside the plugins tree.
fn registry_names_valid(registry: &Registry) -> Result<(), String> {
    for entry in &registry.marketplace {
        if !valid_plugin_name(&entry.name) {
            return Err(format!("`{}` is not a marketplace name", entry.name));
        }
    }
    for entry in &registry.plugin {
        if !valid_plugin_name(&entry.name) {
            return Err(format!("`{}` is not a plugin name", entry.name));
        }
        if !valid_plugin_name(&entry.marketplace) {
            return Err(format!("`{}` is not a marketplace name", entry.marketplace));
        }
    }
    Ok(())
}

/// Plugin and marketplace names: letters, digits, `_` and `-`, starting alphanumeric, never
/// containing `__` (the separator MCP tool names split on). `.` stays out so a composed MCP
/// server name (`plugin_<name>_<server>`) is always a valid rule and tool name.
pub fn valid_plugin_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && !name.contains("__")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn plugin_origin() -> Origin {
    Origin {
        slot: Slot::Plugin,
        user_trust: false,
    }
}

/// Loads the installed, enabled plugins from the registry. A missing registry loads nothing; a
/// corrupt one loads nothing and warns once; a plugin whose checkout vanished contributes
/// nothing and warns once. A plugin whose manifest or component no longer validates is the
/// error.
pub fn load_installed(
    config_dir: &Path,
    ctx: &PluginContext<'_>,
) -> Result<PluginContributions, PluginError> {
    let path = registry_path(config_dir);
    let text = match crate::file_lock::read_shared(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PluginContributions::default());
        }
        Err(e) => {
            return Ok(PluginContributions {
                warnings: vec![format!(
                    "{} cannot be read ({}): no plugin loads until it is fixed or removed",
                    path.display(),
                    fontaine_tools::error_chain::chain(&e)
                )],
                ..PluginContributions::default()
            });
        }
    };
    let registry: Registry = match toml::from_str(&text) {
        Ok(registry) => registry,
        Err(e) => {
            return Ok(PluginContributions {
                warnings: vec![format!(
                    "{} cannot be used ({}): no plugin loads until it is fixed or removed",
                    path.display(),
                    crate::config::bare_toml_message(&text, e.span(), e.message())
                )],
                ..PluginContributions::default()
            });
        }
    };
    if let Err(message) = registry_names_valid(&registry) {
        return Ok(PluginContributions {
            warnings: vec![format!(
                "{} cannot be used ({message}): no plugin loads until it is fixed or removed",
                path.display()
            )],
            ..PluginContributions::default()
        });
    }

    let mut out = PluginContributions::default();
    for entry in registry.plugin.iter().filter(|entry| entry.enabled) {
        let root = checkout_dir(config_dir, &entry.marketplace, &entry.name);
        if !root.is_dir() {
            out.warnings.push(format!(
                "plugin `{}` is installed but its checkout {} is missing; it contributes nothing",
                entry.name,
                root.display()
            ));
            continue;
        }
        let build = build_plugin(&root, &entry.name, ctx)?;
        for name in build.servers.keys() {
            if out.servers.contains_key(name) {
                return Err(PluginError::in_plugin(
                    &entry.name,
                    root.join(MANIFEST_FILE),
                    format!(
                        "its MCP server `{name}` collides with another installed plugin's server"
                    ),
                ));
            }
        }
        out.dirs.push(build.dir);
        out.hooks.pre_tool_use.extend(build.hooks.pre_tool_use);
        out.hooks.post_tool_use.extend(build.hooks.post_tool_use);
        out.hooks
            .permission_denied
            .extend(build.hooks.permission_denied);
        out.servers.extend(build.servers);
        out.deny.extend(build.deny);
        out.ask.extend(build.ask);
        out.hard_deny.extend(build.hard_deny);
        out.soft_deny.extend(build.soft_deny);
        out.warnings.extend(build.warnings);
    }
    Ok(out)
}

/// Reads, validates and resolves one installed plugin directory. Used by startup and by the CLI
/// before an install or update is recorded.
pub fn build_plugin(
    root: &Path,
    registry_name: &str,
    ctx: &PluginContext<'_>,
) -> Result<PluginBuild, PluginError> {
    if !valid_plugin_name(registry_name) {
        return Err(PluginError::in_file(
            root.join(MANIFEST_FILE),
            format!("`{registry_name}` is not a plugin name"),
        ));
    }
    let manifest_path = root.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        PluginError::in_plugin(
            registry_name,
            manifest_path.clone(),
            format!("cannot read it: {}", fontaine_tools::error_chain::chain(&e)),
        )
    })?;
    let manifest: Value = serde_json::from_str(&text).map_err(|e| {
        PluginError::in_plugin(
            registry_name,
            manifest_path.clone(),
            format!("not JSON: {e}"),
        )
    })?;
    let object = manifest.as_object().ok_or_else(|| {
        PluginError::in_plugin(
            registry_name,
            manifest_path.clone(),
            "the manifest is not a JSON object",
        )
    })?;

    let mut warnings = Vec::new();
    for key in ["lspServers", "dependencies", "userConfig", "settings"] {
        if object.contains_key(key) {
            warn_once(
                &mut warnings,
                format!("plugin `{registry_name}` carries `{key}`, which Fontaine does not use"),
            );
        }
    }
    for dir in ["lspServers", "bin", "themes", "monitors", "workflows"] {
        if root.join(dir).exists() {
            warn_once(
                &mut warnings,
                format!(
                    "plugin `{registry_name}` has a `{dir}/` component, which Fontaine does not use"
                ),
            );
        }
    }
    if root.join("settings.json").is_file() {
        warn_once(
            &mut warnings,
            format!("plugin `{registry_name}` has a `settings.json`, which Fontaine does not use"),
        );
    }
    if root.join("channels").exists() {
        warn_once(
            &mut warnings,
            format!(
                "plugin `{registry_name}` has a `channels` component, which Fontaine does not use"
            ),
        );
    }

    let name = object
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            PluginError::in_plugin(
                registry_name,
                manifest_path.clone(),
                "the manifest has no `name`",
            )
        })?;
    if name != registry_name {
        return Err(PluginError::in_plugin(
            registry_name,
            manifest_path.clone(),
            format!("the manifest names `{name}`, not the catalog entry `{registry_name}`"),
        ));
    }

    let mut build = PluginBuild::default();
    build.dir.name = registry_name.to_string();
    build.dir.root = root.to_path_buf();
    let mut env_vars = BTreeSet::new();

    let canonical_root = root.canonicalize().map_err(|e| {
        PluginError::in_plugin(
            registry_name,
            root.to_path_buf(),
            format!(
                "cannot resolve it: {}",
                fontaine_tools::error_chain::chain(&e)
            ),
        )
    })?;

    let mcp_paths = manifest_file_paths(
        object,
        "mcpServers",
        registry_name,
        &manifest_path,
        &mut warnings,
    )?;
    let hook_paths = manifest_file_paths(
        object,
        "hooks",
        registry_name,
        &manifest_path,
        &mut warnings,
    )?;
    let mcp_extra = resolved_component_paths(&canonical_root, root, registry_name, mcp_paths)?;
    let hook_extra = resolved_component_paths(&canonical_root, root, registry_name, hook_paths)?;

    let mcp = parse_mcp(
        root,
        registry_name,
        ctx,
        &mut warnings,
        &mut env_vars,
        &mcp_extra,
    )?;
    for (bare, composed) in &mcp.composed {
        build.dir.mcp_names.insert(bare.clone(), composed.clone());
    }
    build.servers = mcp.servers;
    build.summary.servers = build.servers.len();

    build.hooks = parse_hooks(
        root,
        registry_name,
        ctx,
        &build.dir.mcp_names,
        &mut warnings,
        &mut env_vars,
        &hook_extra,
    )?;
    build.summary.hooks = build.hooks.len();
    build.env_vars = env_vars;

    let permissions = parse_permissions(&manifest, registry_name, &manifest_path)?;
    build.deny = permissions.deny;
    build.ask = permissions.ask;
    build.hard_deny = permissions.hard_deny;
    build.soft_deny = permissions.soft_deny;
    build.summary.deny = build.deny.len();
    build.summary.ask = build.ask.len();
    build.summary.hard_deny = build.hard_deny.len();
    build.summary.soft_deny = build.soft_deny.len();

    // Skills: the default directory plus every manifest `skills` path.
    let mut skill_bases = Vec::new();
    if root.join("skills").is_dir() {
        skill_bases.push(inside(
            &canonical_root,
            &root.join("skills"),
            registry_name,
        )?);
    }
    if let Some(extra) = strings_of(object, "skills", registry_name, &manifest_path)? {
        for rel in extra {
            skill_bases.push(inside(&canonical_root, &root.join(&rel), registry_name)?);
        }
    }
    for base in skill_bases {
        for file in skill_components(&base) {
            build.dir.skills.push(file);
        }
    }
    build.summary.skills = build.dir.skills.len();

    // Commands, agents and output styles: a manifest path replaces the default scan.
    for (key, default, into) in [
        ("commands", "commands", &mut build.dir.commands),
        ("agents", "agents", &mut build.dir.agents),
        ("outputStyles", "output-styles", &mut build.dir.styles),
    ] {
        let bases = match strings_of(object, key, registry_name, &manifest_path)? {
            Some(list) => list
                .into_iter()
                .map(|rel| inside(&canonical_root, &root.join(&rel), registry_name))
                .collect::<Result<Vec<_>, _>>()?,
            None => {
                let default = root.join(default);
                if default.is_dir() {
                    vec![inside(&canonical_root, &default, registry_name)?]
                } else {
                    Vec::new()
                }
            }
        };
        for base in bases {
            if key == "outputStyles" {
                for file in markdown_files(&base, &[]) {
                    into.push(file);
                }
            } else {
                let mut files = Vec::new();
                walk_markdown(&base, &mut Vec::new(), &mut files);
                into.extend(files);
            }
        }
    }
    build.summary.agents = build.dir.agents.len();
    build.summary.styles = build.dir.styles.len();
    build.summary.skills += build.dir.commands.len();

    build.warnings = warnings;
    Ok(build)
}

/// A path the manifest names must stay inside the plugin checkout.
fn inside(root: &Path, path: &Path, plugin: &str) -> Result<PathBuf, PluginError> {
    let resolved = path.canonicalize().map_err(|e| {
        PluginError::in_plugin(
            plugin,
            path.to_path_buf(),
            format!(
                "cannot resolve it: {}",
                fontaine_tools::error_chain::chain(&e)
            ),
        )
    })?;
    if !resolved.starts_with(root) {
        return Err(PluginError::in_plugin(
            plugin,
            path.to_path_buf(),
            "a component path points outside the plugin",
        ));
    }
    Ok(resolved)
}

/// The manifest `hooks` / `mcpServers` key: a path or list of paths naming files that merge
/// over the default file. Inline config objects are Claude Code's other form; Fontaine loads
/// only the path form, so an inline object contributes nothing and warns.
fn manifest_file_paths(
    object: &serde_json::Map<String, Value>,
    key: &str,
    plugin: &str,
    path: &Path,
    warnings: &mut Vec<String>,
) -> Result<Option<Vec<String>>, PluginError> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    if let Some(text) = value.as_str() {
        return Ok(Some(vec![text.to_string()]));
    }
    let Some(items) = value.as_array() else {
        warn_once(
            warnings,
            format!(
                "plugin `{plugin}` has an inline `{key}` manifest key, which Fontaine does not load; it contributes nothing"
            ),
        );
        return Ok(None);
    };
    let mut out = Vec::new();
    for item in items {
        let Some(text) = item.as_str() else {
            return Err(PluginError::in_plugin(
                plugin,
                path.to_path_buf(),
                format!("a `{key}` component path is not a string"),
            ));
        };
        out.push(text.to_string());
    }
    Ok(Some(out))
}

/// Resolves manifest-named component paths, keeping each inside the plugin checkout.
fn resolved_component_paths(
    canonical_root: &Path,
    checkout: &Path,
    plugin: &str,
    rels: Option<Vec<String>>,
) -> Result<Vec<PathBuf>, PluginError> {
    let Some(rels) = rels else {
        return Ok(Vec::new());
    };
    rels.into_iter()
        .map(|rel| inside(canonical_root, &checkout.join(rel), plugin))
        .collect()
}

/// A manifest component-path key: one string or an array of strings. A non-string entry is a
/// load error naming the plugin, not something to skip over.
fn strings_of(
    object: &serde_json::Map<String, Value>,
    key: &str,
    plugin: &str,
    path: &Path,
) -> Result<Option<Vec<String>>, PluginError> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    if let Some(text) = value.as_str() {
        return Ok(Some(vec![text.to_string()]));
    }
    let Some(items) = value.as_array() else {
        return Err(PluginError::in_plugin(
            plugin,
            path.to_path_buf(),
            format!("the `{key}` component path is not a string or a list of strings"),
        ));
    };
    let mut out = Vec::new();
    for item in items {
        let Some(text) = item.as_str() else {
            return Err(PluginError::in_plugin(
                plugin,
                path.to_path_buf(),
                format!("a `{key}` component path is not a string"),
            ));
        };
        out.push(text.to_string());
    }
    Ok(Some(out))
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = read.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    paths
}

/// Skills at a base the manifest or default layout names: a file is one skill, a directory
/// holds `<name>/SKILL.md` subdirectories.
fn skill_components(base: &Path) -> Vec<PluginFile> {
    if base.is_file() {
        return (base.extension().is_some_and(|e| e == "md"))
            .then(|| PluginFile {
                path: base.to_path_buf(),
                segments: Vec::new(),
            })
            .into_iter()
            .collect();
    }
    sorted_entries(base)
        .into_iter()
        .filter(|p| p.is_dir())
        .filter_map(|child| {
            let file = child.join(crate::extensions::SKILL_FILE);
            file.is_file().then_some(PluginFile {
                path: file,
                segments: Vec::new(),
            })
        })
        .collect()
}

fn markdown_files(dir: &Path, segments: &[String]) -> Vec<PluginFile> {
    if dir.is_file() {
        return vec![PluginFile {
            path: dir.to_path_buf(),
            segments: segments.to_vec(),
        }];
    }
    sorted_entries(dir)
        .into_iter()
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md"))
        .map(|path| PluginFile {
            path,
            segments: segments.to_vec(),
        })
        .collect()
}

fn walk_markdown(dir: &Path, segments: &mut Vec<String>, out: &mut Vec<PluginFile>) {
    if dir.is_file() {
        if dir.extension().is_some_and(|e| e == "md") {
            out.push(PluginFile {
                path: dir.to_path_buf(),
                segments: segments.clone(),
            });
        }
        return;
    }
    for entry in sorted_entries(dir) {
        if entry.is_dir() {
            let name = entry
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            segments.push(name);
            walk_markdown(&entry, segments, out);
            segments.pop();
        } else if entry.is_file() && entry.extension().is_some_and(|e| e == "md") {
            out.push(PluginFile {
                path: entry,
                segments: segments.clone(),
            });
        }
    }
}

struct McpLoad {
    servers: BTreeMap<String, McpServer>,
    composed: BTreeMap<String, String>,
}

fn parse_mcp(
    root: &Path,
    plugin: &str,
    ctx: &PluginContext<'_>,
    warnings: &mut Vec<String>,
    env_vars: &mut BTreeSet<String>,
    extra: &[PathBuf],
) -> Result<McpLoad, PluginError> {
    let mut load = parse_mcp_file(&root.join(MCP_FILE), root, plugin, ctx, warnings, env_vars)?;
    for path in extra {
        let more = parse_mcp_file(path, root, plugin, ctx, warnings, env_vars)?;
        load.merge(more);
    }
    Ok(load)
}

/// Reads one `.mcp.json`-format file into named, composed servers. A missing file contributes
/// nothing. The default file and manifest-named files merge; a later file replaces a server of
/// the same name.
fn parse_mcp_file(
    path: &Path,
    root: &Path,
    plugin: &str,
    ctx: &PluginContext<'_>,
    warnings: &mut Vec<String>,
    env_vars: &mut BTreeSet<String>,
) -> Result<McpLoad, PluginError> {
    let mut load = McpLoad {
        servers: BTreeMap::new(),
        composed: BTreeMap::new(),
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(load),
        Err(e) => {
            return Err(PluginError::in_plugin(
                plugin,
                path.to_path_buf(),
                format!("cannot read it: {}", fontaine_tools::error_chain::chain(&e)),
            ));
        }
    };
    let value: Value = serde_json::from_str(&text).map_err(|e| {
        PluginError::in_plugin(plugin, path.to_path_buf(), format!("not JSON: {e}"))
    })?;
    let Some(servers) = value.get("mcpServers").and_then(Value::as_object) else {
        return Ok(load);
    };
    for (name, spec) in servers {
        if name.is_empty()
            || name.contains("__")
            || name.starts_with('_')
            || name.ends_with('_')
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(PluginError::in_plugin(
                plugin,
                path.to_path_buf(),
                format!(
                    "`{name}` is not an MCP server name: it must be non-empty, contain no `__`, neither start nor end with `_`, and use only letters, digits, `_` and `-`"
                ),
            ));
        }
        let object = spec.as_object().ok_or_else(|| {
            PluginError::in_plugin(
                plugin,
                path.to_path_buf(),
                format!("the `.mcp.json` entry `{name}` is not an object"),
            )
        })?;
        let env = match object.get("env") {
            None => None,
            Some(value) => {
                let map = value.as_object().ok_or_else(|| {
                    PluginError::in_plugin(
                        plugin,
                        path.to_path_buf(),
                        format!(
                            "the `.mcp.json` entry `{name}` has an `env` that is not an object"
                        ),
                    )
                })?;
                let mut out = BTreeMap::new();
                for (var, spec) in map {
                    let text = spec.as_str().ok_or_else(|| {
                        PluginError::in_plugin(
                            plugin,
                            path.to_path_buf(),
                            format!(
                                "the `.mcp.json` entry `{name}` has a non-string `env` value for `{var}`"
                            ),
                        )
                    })?;
                    out.insert(
                        var.clone(),
                        expand_vars(text, plugin, root, ctx, warnings, env_vars),
                    );
                }
                Some(out)
            }
        };
        let url = match object.get("url") {
            None => None,
            Some(value) => {
                let text = value.as_str().ok_or_else(|| {
                    PluginError::in_plugin(
                        plugin,
                        path.to_path_buf(),
                        format!("the `.mcp.json` entry `{name}` has a non-string `url`"),
                    )
                })?;
                Some(expand_vars(text, plugin, root, ctx, warnings, env_vars))
            }
        };
        let command = match object.get("command") {
            None => None,
            Some(value) => {
                let text = value.as_str().ok_or_else(|| {
                    PluginError::in_plugin(
                        plugin,
                        path.to_path_buf(),
                        format!("the `.mcp.json` entry `{name}` has a non-string `command`"),
                    )
                })?;
                Some(expand_vars(text, plugin, root, ctx, warnings, env_vars))
            }
        };
        let args = if command.is_some() {
            match object.get("args") {
                None => None,
                Some(value) => {
                    let items = value.as_array().ok_or_else(|| {
                        PluginError::in_plugin(
                            plugin,
                            path.to_path_buf(),
                            format!(
                                "the `.mcp.json` entry `{name}` has an `args` that is not a list"
                            ),
                        )
                    })?;
                    let mut out = Vec::new();
                    for item in items {
                        let text = item.as_str().ok_or_else(|| {
                            PluginError::in_plugin(
                                plugin,
                                path.to_path_buf(),
                                format!("a `.mcp.json` `args` entry of `{name}` is not a string"),
                            )
                        })?;
                        out.push(expand_vars(text, plugin, root, ctx, warnings, env_vars));
                    }
                    Some(out)
                }
            }
        } else {
            None
        };
        let server = match (command, url) {
            (Some(command), None) => McpServer {
                command: Some(command),
                args,
                env_vars: None,
                url: None,
                env,
            },
            (None, Some(_)) if env.is_some() || object.contains_key("args") => {
                return Err(PluginError::in_plugin(
                    plugin,
                    path.to_path_buf(),
                    format!(
                        "the `.mcp.json` entry `{name}` sets `args` or `env`, which apply to stdio servers only"
                    ),
                ));
            }
            (None, Some(url)) => McpServer {
                command: None,
                args: None,
                env_vars: None,
                url: Some(url),
                env: None,
            },
            (Some(_), Some(_)) => {
                return Err(PluginError::in_plugin(
                    plugin,
                    path.to_path_buf(),
                    format!(
                        "the `.mcp.json` entry `{name}` has both `command` and `url`; set exactly one"
                    ),
                ));
            }
            (None, None) => {
                return Err(PluginError::in_plugin(
                    plugin,
                    path.to_path_buf(),
                    format!("the `.mcp.json` entry `{name}` has neither `command` nor `url`"),
                ));
            }
        };
        let composed = format!("plugin_{plugin}_{name}");
        load.composed.insert(name.clone(), composed.clone());
        load.servers.insert(composed, server);
    }
    Ok(load)
}

impl McpLoad {
    fn merge(&mut self, other: McpLoad) {
        for (bare, composed) in other.composed {
            let server = other
                .servers
                .get(&composed)
                .expect("a composed name maps to its server")
                .clone();
            self.composed.insert(bare, composed.clone());
            self.servers.insert(composed, server);
        }
    }
}

fn parse_hooks(
    root: &Path,
    plugin: &str,
    ctx: &PluginContext<'_>,
    mcp_names: &BTreeMap<String, String>,
    warnings: &mut Vec<String>,
    env_vars: &mut BTreeSet<String>,
    extra: &[PathBuf],
) -> Result<PluginHooks, PluginError> {
    let mut hooks = parse_hooks_file(
        &root.join(HOOKS_FILE),
        root,
        plugin,
        ctx,
        mcp_names,
        warnings,
        env_vars,
    )?;
    for path in extra {
        let more = parse_hooks_file(path, root, plugin, ctx, mcp_names, warnings, env_vars)?;
        hooks.pre_tool_use.extend(more.pre_tool_use);
        hooks.post_tool_use.extend(more.post_tool_use);
        hooks.permission_denied.extend(more.permission_denied);
    }
    Ok(hooks)
}

/// Reads one Claude Code `hooks.json` file. A missing file contributes nothing. The default
/// file and manifest-named files merge; both files' entries run for the events they name.
fn parse_hooks_file(
    path: &Path,
    root: &Path,
    plugin: &str,
    ctx: &PluginContext<'_>,
    mcp_names: &BTreeMap<String, String>,
    warnings: &mut Vec<String>,
    env_vars: &mut BTreeSet<String>,
) -> Result<PluginHooks, PluginError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(PluginHooks::default()),
        Err(e) => {
            return Err(PluginError::in_plugin(
                plugin,
                path.to_path_buf(),
                format!("cannot read it: {}", fontaine_tools::error_chain::chain(&e)),
            ));
        }
    };
    let value: Value = serde_json::from_str(&text).map_err(|e| {
        PluginError::in_plugin(plugin, path.to_path_buf(), format!("not JSON: {e}"))
    })?;
    let Some(events) = value.get("hooks").and_then(Value::as_object) else {
        return Ok(PluginHooks::default());
    };
    let mut hooks = PluginHooks::default();
    for (event, entries) in events {
        // Fontaine has exactly these three hook events; the rest are ignored.
        let Some(into) = event_lists(&mut hooks, event) else {
            continue;
        };
        let entries = entries.as_array().ok_or_else(|| {
            PluginError::in_plugin(
                plugin,
                path.to_path_buf(),
                format!("`hooks.{event}` is not an array"),
            )
        })?;
        for entry in entries {
            let entry_object = entry.as_object().ok_or_else(|| {
                PluginError::in_plugin(
                    plugin,
                    path.to_path_buf(),
                    format!("a `hooks.{event}` entry is not an object"),
                )
            })?;
            let matcher = entry_object
                .get("matcher")
                .and_then(Value::as_str)
                .map(str::to_string);
            let specs = entry_object
                .get("hooks")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    PluginError::in_plugin(
                        plugin,
                        path.to_path_buf(),
                        format!("a `hooks.{event}` entry has no `hooks` array"),
                    )
                })?;
            for spec in specs {
                let object = spec.as_object().ok_or_else(|| {
                    PluginError::in_plugin(
                        plugin,
                        path.to_path_buf(),
                        format!("a `hooks.{event}` hook is not an object"),
                    )
                })?;
                let kind = object
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("command");
                if kind != "command" {
                    return Err(PluginError::in_plugin(
                        plugin,
                        path.to_path_buf(),
                        format!(
                            "a `{event}` hook of type `{kind}` is not supported; only shell-form `command` hooks load"
                        ),
                    ));
                }
                if object.contains_key("args") {
                    warn_once(
                        warnings,
                        format!(
                            "plugin `{plugin}` has an exec-form `{event}` hook, which Fontaine does not run; it is skipped"
                        ),
                    );
                    continue;
                }
                let command = object
                    .get("command")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        PluginError::in_plugin(
                            plugin,
                            path.to_path_buf(),
                            format!("a `{event}` hook has no `command`"),
                        )
                    })?;
                let matcher = match matcher.as_deref() {
                    None => None,
                    Some(text) => match map_hook_matcher(text, mcp_names) {
                        Ok(Some(rules)) => {
                            for rule in rules.iter().skip(1) {
                                into.push(Hook {
                                    matcher: Some(rule.clone()),
                                    command: expand_vars(
                                        command, plugin, root, ctx, warnings, env_vars,
                                    ),
                                    timeout_secs: object.get("timeout").and_then(Value::as_u64),
                                    origin: plugin_origin(),
                                    plugin: Some(plugin.to_string()),
                                });
                            }
                            Some(rules[0].clone())
                        }
                        Ok(None) => {
                            warn_once(
                                warnings,
                                format!(
                                    "plugin `{plugin}` has a `{event}` hook matcher `{text}` Fontaine cannot map; its hook is skipped"
                                ),
                            );
                            continue;
                        }
                        Err(message) => {
                            return Err(PluginError::in_plugin(
                                plugin,
                                path.to_path_buf(),
                                format!(
                                    "the `{event}` hook matcher `{text}` is malformed: {message}"
                                ),
                            ));
                        }
                    },
                };
                into.push(Hook {
                    matcher,
                    command: expand_vars(command, plugin, root, ctx, warnings, env_vars),
                    timeout_secs: object.get("timeout").and_then(Value::as_u64),
                    origin: plugin_origin(),
                    plugin: Some(plugin.to_string()),
                });
            }
        }
    }
    Ok(hooks)
}

/// The hook list one Claude Code hook event maps onto; unknown events map to nothing.
fn event_lists<'a>(hooks: &'a mut PluginHooks, event: &str) -> Option<&'a mut Vec<Hook>> {
    match event {
        "PreToolUse" => Some(&mut hooks.pre_tool_use),
        "PostToolUse" => Some(&mut hooks.post_tool_use),
        "PermissionDenied" => Some(&mut hooks.permission_denied),
        _ => None,
    }
}

/// Maps a Claude Code hook matcher to permission-rule matchers: a single tool name, an
/// alternation of mappable names, or a trailing `.*`/`*` wildcard (the rule's wildcard form).
/// `None` is "outside the supported subset"; an `Err` is a matcher that parsed but maps to
/// nothing Fontaine has.
fn map_hook_matcher(
    matcher: &str,
    mcp_names: &BTreeMap<String, String>,
) -> Result<Option<Vec<String>>, String> {
    if matcher.contains('|') {
        let mut rules = Vec::new();
        for alternative in matcher.split('|') {
            let alternative = alternative.trim();
            let Some(rule) = single_matcher(alternative, mcp_names)? else {
                return Ok(None);
            };
            rules.push(rule);
        }
        return Ok(Some(rules));
    }
    Ok(single_matcher(matcher, mcp_names)?.map(|rule| vec![rule]))
}

fn single_matcher(
    matcher: &str,
    mcp_names: &BTreeMap<String, String>,
) -> Result<Option<String>, String> {
    let (name, wildcard) = match matcher
        .strip_suffix(".*")
        .or_else(|| matcher.strip_suffix('*'))
    {
        Some(name) => (name, true),
        None => (matcher, false),
    };
    let name = name.trim();
    let rewritten = rewrite_mcp_reference(name, mcp_names);
    if wildcard {
        // Only an `mcp__<server>` or `mcp__<server>__` prefix has a wildcard rule form.
        let rule =
            if let Some(rest) = rewritten.strip_prefix(fontaine_tools::names::MCP_TOOL_PREFIX) {
                if let Some(server) = rest.strip_suffix("__") {
                    format!("{}{server}__*", fontaine_tools::names::MCP_TOOL_PREFIX)
                } else if !rest.is_empty() && !rest.contains("__") {
                    rewritten
                } else {
                    return Ok(None);
                }
            } else {
                return Ok(None);
            };
        parse_rule(&rule).map_err(|e| e.to_string())?;
        return Ok(Some(rule));
    }
    if is_mcp_tool_name(&rewritten) {
        parse_rule(&rewritten).map_err(|e| e.to_string())?;
        return Ok(Some(rewritten));
    }
    if rewritten.contains('(') {
        return Err("hook matchers name a tool, not a rule specifier".to_string());
    }
    match crate::extensions::map_tool_entry(&rewritten) {
        Ok(rule) => Ok(Some(rule)),
        // A tool Fontaine does not have is an un-mappable matcher, which skips its hook with a
        // warning; it is not a malformed file.
        Err(_) => Ok(None),
    }
}

/// Rewrites a bare `mcp__<server>[__<tool>]` reference to the composed name when the plugin
/// ships the server.
pub(crate) fn rewrite_mcp_reference(entry: &str, mcp_names: &BTreeMap<String, String>) -> String {
    let Some(rest) = entry.strip_prefix(fontaine_tools::names::MCP_TOOL_PREFIX) else {
        return entry.to_string();
    };
    let (server, tail) = match rest.split_once("__") {
        Some((server, tail)) => (server, format!("__{tail}")),
        None => (rest, String::new()),
    };
    match mcp_names.get(server) {
        Some(composed) => format!("{}{composed}{tail}", fontaine_tools::names::MCP_TOOL_PREFIX),
        None => entry.to_string(),
    }
}

fn is_mcp_tool_name(name: &str) -> bool {
    fontaine_tools::names::is_mcp(name)
}

struct PluginRules {
    deny: Vec<SourcedRule>,
    ask: Vec<SourcedRule>,
    hard_deny: Vec<String>,
    soft_deny: Vec<String>,
}

fn parse_permissions(
    manifest: &Value,
    plugin: &str,
    path: &Path,
) -> Result<PluginRules, PluginError> {
    let object = manifest.as_object().expect("the manifest is an object");
    let mut out = PluginRules {
        deny: Vec::new(),
        ask: Vec::new(),
        hard_deny: Vec::new(),
        soft_deny: Vec::new(),
    };
    if let Some(permissions) = object.get("permissions") {
        let map = permissions.as_object().ok_or_else(|| {
            PluginError::in_plugin(plugin, path.to_path_buf(), "`permissions` is not an object")
        })?;
        if map.contains_key("allow") {
            return Err(PluginError::in_plugin(
                plugin,
                path.to_path_buf(),
                "`permissions.allow` is not allowed in a plugin: a plugin cannot loosen permissions",
            ));
        }
        for (key, into) in [("deny", &mut out.deny), ("ask", &mut out.ask)] {
            if let Some(entries) = map.get(key) {
                for text in entries.as_array().ok_or_else(|| {
                    PluginError::in_plugin(
                        plugin,
                        path.to_path_buf(),
                        format!("`permissions.{key}` is not a list"),
                    )
                })? {
                    let text = text.as_str().ok_or_else(|| {
                        PluginError::in_plugin(
                            plugin,
                            path.to_path_buf(),
                            format!("`permissions.{key}` has a non-string entry"),
                        )
                    })?;
                    let rule = parse_rule(text).map_err(|e| {
                        PluginError::in_plugin(
                            plugin,
                            path.to_path_buf(),
                            format!("`permissions.{key}`: {e}"),
                        )
                    })?;
                    into.push(SourcedRule::new(rule, RuleSource::Plugin));
                }
            }
        }
    }
    if let Some(guardian) = object.get("guardian") {
        let map = guardian.as_object().ok_or_else(|| {
            PluginError::in_plugin(plugin, path.to_path_buf(), "`guardian` is not an object")
        })?;
        if map.contains_key("allow") {
            return Err(PluginError::in_plugin(
                plugin,
                path.to_path_buf(),
                "`guardian.allow` is not allowed in a plugin: a plugin cannot loosen the guardian",
            ));
        }
        if map.contains_key("environment") {
            return Err(PluginError::in_plugin(
                plugin,
                path.to_path_buf(),
                "`guardian.environment` is not allowed in a plugin: a plugin cannot change the guardian's environment",
            ));
        }
        for (key, into) in [
            ("hard_deny", &mut out.hard_deny),
            ("soft_deny", &mut out.soft_deny),
        ] {
            if let Some(entries) = map.get(key) {
                for text in entries.as_array().ok_or_else(|| {
                    PluginError::in_plugin(
                        plugin,
                        path.to_path_buf(),
                        format!("`guardian.{key}` is not a list"),
                    )
                })? {
                    let text = text.as_str().ok_or_else(|| {
                        PluginError::in_plugin(
                            plugin,
                            path.to_path_buf(),
                            format!("`guardian.{key}` has a non-string entry"),
                        )
                    })?;
                    into.push(text.to_string());
                }
            }
        }
    }
    Ok(out)
}

/// Expands `${CLAUDE_PLUGIN_ROOT}`, `${CLAUDE_PROJECT_DIR}` and `${VAR}` from the environment;
/// any other `${...}` reference stays literal and warns once.
pub(crate) fn expand_vars(
    text: &str,
    plugin: &str,
    plugin_root: &Path,
    ctx: &PluginContext<'_>,
    warnings: &mut Vec<String>,
    env_vars: &mut BTreeSet<String>,
) -> String {
    let root = plugin_root.display().to_string();
    let project = ctx.project_root.display().to_string();
    let mut out = String::new();
    let mut at = 0;
    while at < text.len() {
        if text[at..].starts_with("${")
            && let Some(end) = text[at + 2..].find('}')
        {
            let name = &text[at + 2..at + 2 + end];
            match name {
                "CLAUDE_PLUGIN_ROOT" => out.push_str(&root),
                "CLAUDE_PROJECT_DIR" => out.push_str(&project),
                _ => {
                    env_vars.insert(name.to_string());
                    match (ctx.env)(name) {
                        Some(value) => out.push_str(&value.to_string_lossy()),
                        None => {
                            out.push_str(&text[at..at + 2 + end + 1]);
                            warn_once(
                                warnings,
                                format!("plugin `{plugin}` leaves `${{{name}}}` unexpanded"),
                            );
                        }
                    }
                }
            }
            at += 2 + end + 1;
            continue;
        }
        let ch = text[at..].chars().next().expect("at is a char boundary");
        out.push(ch);
        at += ch.len_utf8();
    }
    out
}

fn warn_once(warnings: &mut Vec<String>, message: String) {
    if !warnings.contains(&message) {
        warnings.push(message);
    }
}

/// Parses a marketplace catalog, naming the file in its errors.
pub fn parse_catalog(text: &str, path: &Path) -> Result<Catalog, PluginError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|e| PluginError::in_file(path.to_path_buf(), format!("not JSON: {e}")))?;
    let object = value.as_object().ok_or_else(|| {
        PluginError::in_file(path.to_path_buf(), "the catalog is not a JSON object")
    })?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| PluginError::in_file(path.to_path_buf(), "the catalog has no `name`"))?;
    if !valid_plugin_name(&name) {
        return Err(PluginError::in_file(
            path.to_path_buf(),
            format!("`{name}` is not a marketplace name"),
        ));
    }
    let entries = object
        .get("plugins")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            PluginError::in_file(path.to_path_buf(), "the catalog has no `plugins` array")
        })?;
    let mut plugins = Vec::new();
    for entry in entries {
        let entry = entry.as_object().ok_or_else(|| {
            PluginError::in_file(path.to_path_buf(), "a `plugins` entry is not an object")
        })?;
        let plugin_name = entry
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                PluginError::in_file(path.to_path_buf(), "a `plugins` entry has no `name`")
            })?;
        if !valid_plugin_name(&plugin_name) {
            return Err(PluginError::in_file(
                path.to_path_buf(),
                format!("`{plugin_name}` is not a plugin name"),
            ));
        }
        let source = parse_source(entry.get("source"), path)?;
        plugins.push(CatalogPlugin {
            name: plugin_name,
            source,
            description: entry
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            version: entry
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
    Ok(Catalog { name, plugins })
}

/// A `ref`, `sha` or `url` that reaches `git` as a positional argument. git reads a leading `-`
/// as an option, so such a value is refused here rather than at the shell-out.
fn git_positional_ok(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('-') && !value.chars().any(char::is_whitespace)
}

fn parse_source(value: Option<&Value>, path: &Path) -> Result<CatalogSource, PluginError> {
    let Some(value) = value else {
        return Err(PluginError::in_file(
            path.to_path_buf(),
            "a `plugins` entry has no `source`",
        ));
    };
    if let Some(rel) = value.as_str() {
        return Ok(CatalogSource::Path(rel.to_string()));
    }
    let object = value.as_object().ok_or_else(|| {
        PluginError::in_file(
            path.to_path_buf(),
            "a plugin `source` must be a relative path or an object",
        )
    })?;
    let kind = object
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            PluginError::in_file(
                path.to_path_buf(),
                "a plugin `source` object has no `source`",
            )
        })?;
    let r#ref = object
        .get("ref")
        .and_then(Value::as_str)
        .map(str::to_string);
    let sha = object
        .get("sha")
        .and_then(Value::as_str)
        .map(str::to_string);
    if r#ref
        .as_deref()
        .is_some_and(|value| !git_positional_ok(value))
    {
        return Err(PluginError::in_file(
            path.to_path_buf(),
            "a plugin `source` `ref` must be a non-empty branch or tag name",
        ));
    }
    if sha
        .as_deref()
        .is_some_and(|value| !git_positional_ok(value))
    {
        return Err(PluginError::in_file(
            path.to_path_buf(),
            "a plugin `source` `sha` must be a non-empty commit hash",
        ));
    }
    match kind {
        "github" => {
            let repo = object.get("repo").and_then(Value::as_str).ok_or_else(|| {
                PluginError::in_file(path.to_path_buf(), "a `github` source has no `repo`")
            })?;
            if !git_positional_ok(repo) {
                return Err(PluginError::in_file(
                    path.to_path_buf(),
                    "a `github` source `repo` must be a non-empty `owner/repo`",
                ));
            }
            Ok(CatalogSource::Github {
                repo: repo.to_string(),
                r#ref,
                sha,
            })
        }
        "url" => {
            let url = object.get("url").and_then(Value::as_str).ok_or_else(|| {
                PluginError::in_file(path.to_path_buf(), "a `url` source has no `url`")
            })?;
            if !git_positional_ok(url) {
                return Err(PluginError::in_file(
                    path.to_path_buf(),
                    "a plugin `source` `url` must be a non-empty git URL",
                ));
            }
            Ok(CatalogSource::Url {
                url: url.to_string(),
                r#ref,
                sha,
            })
        }
        other => Err(PluginError::in_file(
            path.to_path_buf(),
            format!(
                "the plugin source type `{other}` is not supported; Fontaine installs `github`, `url` and relative-path sources only"
            ),
        )),
    }
}

impl crate::config::Config {
    /// Merges plugin hooks, servers and deny/ask rules into the resolved configuration. Plugin
    /// entries only tighten: hooks carry `user_trust: false` so their `allow`/`retry` never
    /// counts, and servers start without the workspace-trust gate (install approved them). A
    /// composed server name that collides with a configured non-plugin server is an error naming
    /// the plugin. Re-applying replaces the previous plugin contributions, so the
    /// session-switch path, which reuses the already-resolved config, converges to the current
    /// registry rather than accumulating removed or changed plugins.
    pub fn apply_plugins(&mut self, plugins: &PluginContributions) -> Result<(), PluginError> {
        // Strip the previous application first: the session-switch path reuses the resolved
        // config, so a plugin removed, disabled or changed since the last application must not
        // leave its old hooks, servers or rules behind. A hook the plugin only lent its
        // attribution to (an identical hook of another scope) survives; its attribution is
        // cleared and the re-application below re-derives it.
        for hooks in [
            &mut self.hooks.pre_tool_use,
            &mut self.hooks.post_tool_use,
            &mut self.hooks.permission_denied,
        ] {
            hooks.retain(|hook| hook.origin.slot != Slot::Plugin);
            for hook in hooks.iter_mut() {
                hook.plugin = None;
            }
        }
        let plugin_servers: BTreeSet<&str> = self
            .mcp_origins
            .iter()
            .filter(|(_, origin)| origin.slot == Slot::Plugin)
            .map(|(name, _)| name.as_str())
            .collect();
        self.mcp_servers
            .retain(|name, _| !plugin_servers.contains(name.as_str()));
        self.mcp_origins
            .retain(|_, origin| origin.slot != Slot::Plugin);
        self.permissions
            .deny
            .retain(|entry| entry.source != RuleSource::Plugin);
        self.permissions
            .ask
            .retain(|entry| entry.source != RuleSource::Plugin);
        self.plugin_hard_deny = plugins.hard_deny.clone();
        self.plugin_soft_deny = plugins.soft_deny.clone();

        for name in plugins.servers.keys() {
            if self.mcp_servers.contains_key(name) {
                let plugin = plugins
                    .dirs
                    .iter()
                    .find(|dir| name.starts_with(&format!("plugin_{}_", dir.name)))
                    .map(|dir| dir.name.as_str())
                    .unwrap_or(name);
                return Err(PluginError {
                    plugin: Some(plugin.to_string()),
                    file: PathBuf::from(crate::config::USER_FILE),
                    message: format!(
                        "its MCP server `{name}` collides with a configured non-plugin server"
                    ),
                });
            }
        }
        let origin = plugin_origin();
        crate::config::union_hooks(
            &mut self.hooks.pre_tool_use,
            Some(&plugins.hooks.pre_tool_use),
            origin,
        );
        crate::config::union_hooks(
            &mut self.hooks.post_tool_use,
            Some(&plugins.hooks.post_tool_use),
            origin,
        );
        crate::config::union_hooks(
            &mut self.hooks.permission_denied,
            Some(&plugins.hooks.permission_denied),
            origin,
        );
        for (name, server) in &plugins.servers {
            self.mcp_servers.insert(name.clone(), server.clone());
            self.mcp_origins.insert(name.clone(), origin);
        }
        for entry in &plugins.deny {
            if !self.permissions.deny.contains(entry) {
                self.permissions.deny.push(entry.clone());
            }
        }
        for entry in &plugins.ask {
            if !self.permissions.ask.contains(entry) {
                self.permissions.ask.push(entry.clone());
            }
        }
        Ok(())
    }
}

/// A stable content hash of a directory tree, `.git` excluded: a local-directory marketplace's
/// pin. A content change moves the pin even when the catalog file is untouched; a metadata-only
/// git operation does not. Symlinks are followed, so the hash covers what `copy_tree` copies;
/// directory cycles through symlinks are cut at the second visit.
pub fn content_hash(dir: &Path) -> Result<String, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    let mut visited = std::collections::HashSet::new();
    while let Some(current) = stack.pop() {
        let entries = std::fs::read_dir(&current)
            .map_err(|e| format!("cannot read {}: {}", current.display(), e))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if name == ".git" {
                continue;
            }
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            // `symlink_metadata`-based `file_type` does not follow links; `metadata` does,
            // matching the copy that `copy_tree` makes of the tree.
            let (is_dir, is_file) = if kind.is_symlink() {
                let followed = std::fs::metadata(&path)
                    .map_err(|e| format!("cannot follow {}: {}", path.display(), e))?;
                (followed.is_dir(), followed.is_file())
            } else {
                (kind.is_dir(), kind.is_file())
            };
            if is_dir {
                let real = path
                    .canonicalize()
                    .map_err(|e| format!("cannot resolve {}: {}", path.display(), e))?;
                if visited.insert(real) {
                    stack.push(path);
                }
            } else if is_file {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut hasher = Sha256::new();
    for file in files {
        let rel = file
            .strip_prefix(dir)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        let bytes =
            std::fs::read(&file).map_err(|e| format!("cannot read {}: {}", file.display(), e))?;
        hasher.update(bytes);
        hasher.update([0]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_pins_never_split_a_character() {
        assert_eq!(short_pin("a1b2c3d4"), "a1b2c3d");
        assert_eq!(short_pin("abc"), "abc");
        assert_eq!(short_pin(""), "");
        // Seven bytes is mid-character for a multibyte pin.
        assert_eq!(short_pin("éééé"), "ééé");
    }

    #[test]
    fn plugin_and_marketplace_names_follow_the_alphabet() {
        for name in ["review", "team-tools", "A1", "1a", "a_b"] {
            assert!(valid_plugin_name(name), "{name}");
        }
        for name in ["", ".x", "-x", "a.b", "a__b", "a b", "a/b", "a:b"] {
            assert!(!valid_plugin_name(name), "{name}");
        }
    }

    #[test]
    fn variable_expansion_and_unexpanded_warnings() {
        let project = Path::new("/work");
        let env = |name: &str| (name == "TOKEN").then(|| OsString::from("t0ken"));
        let ctx = PluginContext {
            project_root: project,
            env: &env,
        };
        let mut warnings = Vec::new();
        let mut env_vars = BTreeSet::new();
        let text = "${CLAUDE_PLUGIN_ROOT}/bin ${CLAUDE_PROJECT_DIR} ${TOKEN} ${MISSING}";
        let out = expand_vars(
            text,
            "p",
            Path::new("/cfg/p"),
            &ctx,
            &mut warnings,
            &mut env_vars,
        );
        assert_eq!(out, "/cfg/p/bin /work t0ken ${MISSING}");
        assert_eq!(warnings, vec!["plugin `p` leaves `${MISSING}` unexpanded"]);
        assert_eq!(
            env_vars,
            BTreeSet::from(["TOKEN".to_string(), "MISSING".to_string()])
        );
    }

    #[test]
    fn hook_matchers_map_the_supported_subset() {
        let mut names = BTreeMap::new();
        names.insert("local".to_string(), "plugin_p_local".to_string());
        assert_eq!(
            map_hook_matcher("Bash", &names).unwrap(),
            Some(vec!["shell".to_string()])
        );
        assert_eq!(
            map_hook_matcher("Write|Edit", &names).unwrap(),
            Some(vec!["write".to_string(), "edit".to_string()])
        );
        assert_eq!(
            map_hook_matcher("mcp__gh__.*", &names).unwrap(),
            Some(vec!["mcp__gh__*".to_string()])
        );
        assert_eq!(
            map_hook_matcher("mcp__local__.*", &names).unwrap(),
            Some(vec!["mcp__plugin_p_local__*".to_string()])
        );
        assert!(map_hook_matcher("Bash.*", &names).unwrap().is_none());
        assert!(map_hook_matcher("MadeUp|Bash", &names).unwrap().is_none());
        assert!(map_hook_matcher("Bash(", &names).is_err());
    }

    #[test]
    fn manifest_permission_keys_are_deny_side_only() {
        let path = Path::new("plugin.json");
        let allow = serde_json::json!({"permissions": {"allow": ["read"]}});
        assert!(parse_permissions(&allow, "p", path).is_err());
        let env = serde_json::json!({"guardian": {"environment": ["x"]}});
        assert!(parse_permissions(&env, "p", path).is_err());
        let ok = serde_json::json!({
            "permissions": {"deny": ["shell(git push *)"], "ask": ["read(.env)"]},
            "guardian": {"hard_deny": ["never push on Fridays"], "soft_deny": []}
        });
        let rules = parse_permissions(&ok, "p", path).unwrap();
        assert_eq!(rules.deny.len(), 1);
        assert_eq!(rules.ask.len(), 1);
        assert_eq!(rules.hard_deny, vec!["never push on Fridays"]);
        assert!(rules.soft_deny.is_empty());
        assert!(
            parse_permissions(
                &serde_json::json!({"permissions": {"deny": ["nope"]}}),
                "p",
                path
            )
            .is_err()
        );
    }

    #[test]
    fn catalog_sources_refuse_unsupported_types() {
        let path = Path::new("marketplace.json");
        let catalog = parse_catalog(
            r#"{
                "name": "team",
                "plugins": [
                    {"name": "a", "source": "./plugins/a"},
                    {"name": "b", "source": {"source": "github", "repo": "o/r"}},
                    {"name": "c", "source": {"source": "url", "url": "https://x/y.git", "ref": "main"}}
                ]
            }"#,
            path,
        )
        .unwrap();
        assert_eq!(catalog.plugins.len(), 3);
        assert!(matches!(catalog.plugins[0].source, CatalogSource::Path(_)));
        assert!(matches!(
            catalog.plugins[1].source,
            CatalogSource::Github { .. }
        ));
        assert!(matches!(
            catalog.plugins[2].source,
            CatalogSource::Url { .. }
        ));
        let bad = parse_catalog(
            r#"{"name": "team", "plugins": [{"name": "a", "source": {"source": "npm", "package": "x"}}]}"#,
            path,
        );
        assert!(bad.unwrap_err().to_string().contains("npm"));
    }

    #[test]
    fn catalog_sources_refuse_option_like_git_values() {
        let path = Path::new("marketplace.json");
        for field in ["ref", "sha"] {
            let json = format!(
                r#"{{"name":"t","plugins":[{{"name":"a","source":{{"source":"github","repo":"o/r","{field}":"-x"}}}}]}}"#
            );
            assert!(parse_catalog(&json, path).is_err(), "{field}");
        }
        assert!(
            parse_catalog(
                r#"{"name":"t","plugins":[{"name":"a","source":{"source":"url","url":"-x"}}]}"#,
                path
            )
            .is_err()
        );
        assert!(parse_catalog(
            r#"{"name":"t","plugins":[{"name":"a","source":{"source":"github","repo":"o/r","ref":"main","sha":"a1b2c3"}}]}"#,
            path
        )
        .is_ok());
    }

    #[test]
    fn content_hash_ignores_git_and_metadata() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/config"), "x").unwrap();
        std::fs::write(dir.path().join("catalog.json"), "a").unwrap();
        let first = content_hash(dir.path()).unwrap();
        std::fs::write(dir.path().join("catalog.json"), "b").unwrap();
        let second = content_hash(dir.path()).unwrap();
        assert_ne!(first, second);
        // Metadata-only change: the .git tree is excluded.
        std::fs::write(dir.path().join(".git/config"), "y").unwrap();
        assert_eq!(content_hash(dir.path()).unwrap(), second);
    }

    #[test]
    fn the_registry_round_trips_and_validates_names() {
        let registry = Registry {
            marketplace: vec![MarketplaceEntry {
                name: "team".to_string(),
                url: "./team".to_string(),
                pin: "a1b2c3".to_string(),
            }],
            plugin: vec![InstalledPlugin {
                name: "review".to_string(),
                marketplace: "team".to_string(),
                source: "./plugins/review".to_string(),
                pin: "d4e5f6".to_string(),
                enabled: true,
            }],
        };
        let text = toml::to_string_pretty(&registry).unwrap();
        assert!(validate_plugins_toml(&text).is_ok());
        let bad = text.replace("name = \"review\"", "name = \"a__b\"");
        assert!(validate_plugins_toml(&bad).is_err());
    }
}
