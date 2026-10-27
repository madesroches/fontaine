//! The built-in tool set, each tool wrapped in the gate. File tools and `shell` are rig's own,
//! configured rather than forked: one confined instance per workspace root.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use rig_core::completion::ToolAnnotations;
use rig_core::tool::{DynamicTool, Tool, ToolContext, ToolExecutionError, args_schema};
pub use rig_tools::Seen;
use rig_tools::process::Launcher;
use rig_tools::{
    Edit, List, ListArgs, MAX_FILES, MAX_MATCHES, OutputLimit, Read, Search, SearchArgs, Shell,
    ShellArgs, ShellRun, Write,
};
use serde::Deserialize;
use serde_json::json;

use crate::git_bash::{GitBash, sh_command};
use crate::guarded::{Guard, GuardedTool};
use crate::risk::{RiskClass, ToolSource, registered_risk};
use crate::scope::{Scope, glob_target};
use crate::tls::LoadedNetwork;
use crate::webfetch::{Webfetch, WebfetchConfig};
use crate::websearch::{SearxngChild, Websearch, WebsearchConfig};

/// The directory listing tool; the model sees it as `ls`.
#[derive(Clone, Debug)]
pub struct Ls {
    pub list: List,
}

impl Tool for Ls {
    const NAME: &'static str = "ls";
    type Args = ListArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "List the files under a directory, sorted, one path per line below it. Skips files \
         ignored by git and, unless `hidden` is set or the glob names one, hidden files. With \
         `depth`, directories below that depth are listed once as `dir/`. Use it to see what \
         a directory holds, not `ls` or `find` in `shell`."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        args_schema::<ListArgs>()
    }

    fn annotations(&self) -> ToolAnnotations {
        self.list.annotations()
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: ListArgs,
    ) -> Result<String, ToolExecutionError> {
        self.list.call(context, args).await
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobArgs {
    pub pattern: String,
    pub path: Option<String>,
}

/// Finds files by name with a glob pattern.
#[derive(Clone, Debug)]
pub struct Glob {
    pub search: Search,
}

impl Tool for Glob {
    const NAME: &'static str = "glob";
    type Args = GlobArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        format!(
            "Find files whose path matches a glob pattern such as `src/**/*.rs`. Skips files \
             ignored by git and hidden files unless the pattern names one. Returns one path per \
             line, at most {MAX_FILES}. `path` is the directory to search (default: the working \
             root). Use it to find files by name, not `find` in `shell`."
        )
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "The glob pattern to match, such as `**/*.rs`." },
                "path": { "type": "string", "description": "The directory to search; default the working root." },
            },
            "required": ["pattern"],
            "additionalProperties": false,
        })
    }

    fn annotations(&self) -> ToolAnnotations {
        self.search.annotations()
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: GlobArgs,
    ) -> Result<String, ToolExecutionError> {
        let (base, tail) = glob_target(args.path.as_deref(), &args.pattern);
        let search = SearchArgs {
            pattern: String::new(),
            path: Some(base),
            glob: (!tail.is_empty()).then_some(tail),
            hidden: false,
            files: true,
        };
        self.search.call(context, search).await
    }
}

/// Searches file contents with a regular expression.
#[derive(Clone, Debug)]
pub struct Grep {
    pub search: Search,
}

impl Tool for Grep {
    const NAME: &'static str = "grep";
    type Args = SearchArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        format!(
            "Search file contents with a regular expression (Rust regex syntax; prefix the \
             pattern with `(?i)` to ignore case). Skips files ignored by git and, unless \
             `hidden` is set or the glob names one, hidden files. Returns `path:line: text`, at \
             most {MAX_MATCHES} matches; with `files`, only the paths of the files with a match. \
             Use it to find code, not `grep` or `rg` in `shell`."
        )
    }

    fn parameters(&self) -> serde_json::Value {
        args_schema::<SearchArgs>()
    }

    fn annotations(&self) -> ToolAnnotations {
        self.search.annotations()
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: SearchArgs,
    ) -> Result<String, ToolExecutionError> {
        self.search.call(context, args).await
    }
}

/// rig's `shell`, with the command kept off the Windows command line when it runs through
/// Git Bash.
#[derive(Clone, Debug)]
pub struct ShellTool {
    pub shell: Shell,
    pub through_env: bool,
}

impl ShellTool {
    fn prepared(&self, args: ShellArgs) -> (Shell, ShellArgs) {
        let sh = sh_command(&args.command, self.through_env);
        let Some((name, value)) = sh.env else {
            return (self.shell.clone(), args);
        };
        let mut shell = self.shell.clone();
        shell.launcher = shell.launcher.with_env(name, value);
        let script = sh.script.to_string();
        (
            shell,
            ShellArgs {
                command: script,
                ..args
            },
        )
    }

    pub async fn run(&self, args: ShellArgs) -> Result<ShellRun, ToolExecutionError> {
        let (shell, args) = self.prepared(args);
        shell.run(args).await
    }
}

impl Tool for ShellTool {
    const NAME: &'static str = Shell::NAME;
    type Args = ShellArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        self.shell.description()
    }

    fn parameters(&self) -> serde_json::Value {
        self.shell.parameters()
    }

    fn annotations(&self) -> ToolAnnotations {
        self.shell.annotations()
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: ShellArgs,
    ) -> Result<String, ToolExecutionError> {
        let (shell, args) = self.prepared(args);
        shell.call(context, args).await
    }
}

#[derive(Clone, Debug)]
pub struct ShellSettings {
    /// The longest timeout a call may ask for; a longer one is cut to it.
    pub max_timeout_secs: u64,
    /// Variables the shell's child processes never inherit.
    pub scrub_env: Vec<OsString>,
    /// Variables set after the scrub, from the user's `[env]`.
    pub set_env: Vec<(OsString, OsString)>,
    pub git_bash: Option<GitBash>,
}

impl Default for ShellSettings {
    fn default() -> Self {
        ShellSettings {
            max_timeout_secs: 600,
            scrub_env: Vec::new(),
            set_env: Vec::new(),
            git_bash: None,
        }
    }
}

pub fn read_for(root: &Path, seen: &Seen) -> Read {
    Read {
        base: Some(root.to_path_buf()),
        confine: true,
        seen: seen.clone(),
        ..Read::default()
    }
}

pub fn write_for(root: &Path, seen: &Seen) -> Write {
    Write {
        base: Some(root.to_path_buf()),
        confine: true,
        seen: seen.clone(),
        ..Write::default()
    }
}

pub fn edit_for(root: &Path, seen: &Seen) -> Edit {
    Edit {
        base: Some(root.to_path_buf()),
        confine: true,
        seen: seen.clone(),
        ..Edit::default()
    }
}

pub fn list_for(root: &Path) -> List {
    List {
        base: Some(root.to_path_buf()),
        confine: true,
        ..List::default()
    }
}

pub fn search_for(root: &Path) -> Search {
    Search {
        base: Some(root.to_path_buf()),
        confine: true,
        ..Search::default()
    }
}

pub fn shell_for(root: &Path, settings: &ShellSettings) -> ShellTool {
    let mut launcher = Launcher::default()
        .in_dir(root)
        .without_env(settings.scrub_env.iter().cloned());
    for (name, value) in &settings.set_env {
        launcher = launcher.with_env(name, value);
    }
    if let Some(git_bash) = &settings.git_bash {
        launcher = launcher.with_env(
            "PATH",
            git_bash.path_value(crate::git_bash::base_path(&settings.set_env)),
        );
    }
    ShellTool {
        shell: Shell {
            launcher,
            confine: true,
            max_timeout: settings.max_timeout_secs,
            cut: Some(OutputLimit::default()),
            ..Shell::default()
        },
        through_env: settings.git_bash.is_some(),
    }
}

pub struct BuiltinEnv {
    pub scope: Arc<Scope>,
    /// Read-before-edit tracking shared by every `read` and `edit` instance. Passing the same
    /// one again when the registry is rebuilt keeps what the model has already read.
    pub seen: Seen,
    pub guard: Guard,
    pub shell: ShellSettings,
    pub webfetch: WebfetchConfig,
    /// Present only when a search backend is configured; with none, the tool does not exist.
    pub websearch: Option<WebsearchConfig>,
    /// The SearXNG process a session launches. Passing the same one again when the registry is
    /// rebuilt keeps the instance running.
    pub searxng: Arc<SearxngChild>,
    pub network: Arc<LoadedNetwork>,
}

pub struct Registered {
    pub name: &'static str,
    pub risk: RiskClass,
    pub source: ToolSource,
    pub tool: DynamicTool,
}

/// The risk class a built-in is registered with. A built-in missing from the table is the
/// riskiest class, so a forgotten entry can only make a tool stricter.
fn risk_of(name: &str) -> RiskClass {
    registered_risk(name).unwrap_or(RiskClass::Arbitrary)
}

fn register<T: Tool + 'static>(guarded: GuardedTool<T>) -> Registered {
    Registered {
        name: T::NAME,
        risk: guarded.risk(),
        source: ToolSource::Builtin,
        tool: guarded.into_dynamic(),
    }
}

/// Every built-in tool, wrapped. One `Seen` is shared by all `read` and `edit` instances so
/// read-before-edit tracking holds whichever root serves a call.
pub fn build(env: &BuiltinEnv) -> Result<Vec<Registered>, BuildError> {
    let scope = &env.scope;
    let guard = &env.guard;
    let seen = env.seen.clone();
    let mut tools = vec![
        register(GuardedTool::per_root(
            scope.clone(),
            risk_of(Read::NAME),
            guard.clone(),
            |root| read_for(root, &seen),
        )),
        register(GuardedTool::per_root(
            scope.clone(),
            risk_of(Ls::NAME),
            guard.clone(),
            |root| Ls {
                list: list_for(root),
            },
        )),
        register(GuardedTool::per_root(
            scope.clone(),
            risk_of(Glob::NAME),
            guard.clone(),
            |root| Glob {
                search: search_for(root),
            },
        )),
        register(GuardedTool::per_root(
            scope.clone(),
            risk_of(Grep::NAME),
            guard.clone(),
            |root| Grep {
                search: search_for(root),
            },
        )),
        register(GuardedTool::per_root(
            scope.clone(),
            risk_of(Write::NAME),
            guard.clone(),
            |root| write_for(root, &seen),
        )),
        register(GuardedTool::per_root(
            scope.clone(),
            risk_of(Edit::NAME),
            guard.clone(),
            |root| edit_for(root, &seen),
        )),
        register(GuardedTool::per_root(
            scope.clone(),
            risk_of(ShellTool::NAME),
            guard.clone(),
            |root| shell_for(root, &env.shell),
        )),
        register(GuardedTool::single(
            Webfetch::new(env.webfetch.clone(), &env.network).map_err(|source| BuildError {
                tool: Webfetch::NAME,
                source,
            })?,
            risk_of(Webfetch::NAME),
            guard.clone(),
        )),
    ];
    if let Some(config) = &env.websearch {
        let tool = Websearch::new(config.clone(), &env.network, env.searxng.clone()).map_err(
            |source| BuildError {
                tool: Websearch::NAME,
                source,
            },
        )?;
        tools.push(register(GuardedTool::single(
            tool,
            risk_of(Websearch::NAME),
            guard.clone(),
        )));
    }
    Ok(tools)
}

/// A built-in whose HTTP client could not be built.
#[derive(Debug)]
pub struct BuildError {
    pub tool: &'static str,
    pub source: reqwest::Error,
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot set up the {} tool", self.tool)
    }
}

impl std::error::Error for BuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
