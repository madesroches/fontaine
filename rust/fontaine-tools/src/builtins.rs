//! The built-in tool set, each tool wrapped in the gate. File tools and `shell` are rig's own,
//! configured rather than forked: one confined instance per workspace root.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rig_core::completion::ToolAnnotations;
use rig_core::tool::{DynamicTool, ExitStatus, Tool, ToolContext, ToolExecutionError, args_schema};
pub use rig_tools::Seen;
use rig_tools::process::{Launcher, Process, Spawn};
use rig_tools::{
    Edit, List, ListArgs, MAX_FILES, MAX_MATCHES, OutputLimit, Read, Search, SearchArgs, Shell,
    ShellArgs, ShellRun, Write,
};
use serde::Deserialize;
use serde_json::json;

use crate::error_chain::chain;
use crate::gate::{AgentId, ROOT_AGENT};
use crate::git_bash::{GitBash, sh_command};
use crate::guarded::{Guard, GuardedTool};
use crate::jobs::tools::{JobCancelTool, JobOutputTool, MonitorTool, job_error};
use crate::jobs::{
    AgentWaits, End, JobError, Jobs, Kill, MAX_RUNNING, NoPromote, OUTPUT_CAP, QUIET_LIMIT, Start,
    WaitWake, cut,
};
use crate::notebook::{NOTEBOOK_STEER, NotebookEdit, NotebookRead};
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

/// rig's `write`, pointed away from notebooks in its description.
#[derive(Clone, Debug)]
pub struct WriteTool {
    pub write: Write,
}

impl Tool for WriteTool {
    const NAME: &'static str = Write::NAME;
    type Args = <Write as Tool>::Args;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        self.write.description() + NOTEBOOK_STEER
    }

    fn parameters(&self) -> serde_json::Value {
        self.write.parameters()
    }

    fn annotations(&self) -> ToolAnnotations {
        self.write.annotations()
    }

    fn footprint(&self, args: &Self::Args) -> rig_core::tool::Touches {
        self.write.footprint(args)
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<String, ToolExecutionError> {
        self.write.call(context, args).await
    }
}

/// rig's `edit`, pointed away from notebooks in its description.
#[derive(Clone, Debug)]
pub struct EditTool {
    pub edit: Edit,
}

impl Tool for EditTool {
    const NAME: &'static str = Edit::NAME;
    type Args = <Edit as Tool>::Args;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        self.edit.description() + NOTEBOOK_STEER
    }

    fn parameters(&self) -> serde_json::Value {
        self.edit.parameters()
    }

    fn annotations(&self) -> ToolAnnotations {
        self.edit.annotations()
    }

    fn footprint(&self, args: &Self::Args) -> rig_core::tool::Touches {
        self.edit.footprint(args)
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<String, ToolExecutionError> {
        self.edit.call(context, args).await
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

/// The seconds a call waits before its command moves to the background or is stopped.
const DEFAULT_TIMEOUT: u64 = 120;

/// How long a call may print nothing: only a call with the default timeout has a limit, since
/// one that names its timeout may be quiet as long as that.
fn quiet_for(timeout_secs: Option<u64>, timeout: Duration) -> Option<Duration> {
    match timeout_secs {
        None => Some(QUIET_LIMIT).filter(|quiet| *quiet < timeout),
        Some(_) => None,
    }
}

/// The arguments of `shell`: rig's, and whether the command starts in the background.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellCall {
    pub command: String,
    pub timeout_secs: Option<u64>,
    pub cwd: Option<String>,
    pub run_in_background: Option<bool>,
}

impl ShellCall {
    fn shell_args(&self) -> ShellArgs {
        ShellArgs {
            command: self.command.clone(),
            timeout_secs: self.timeout_secs,
            cwd: self.cwd.clone(),
        }
    }
}

/// rig's `shell`, with the command kept off the Windows command line when it runs through
/// Git Bash. For the main agent a call is a job: one that outlives its timeout moves to the
/// background instead of being stopped.
#[derive(Clone, Debug)]
pub struct ShellTool {
    pub shell: Shell,
    pub through_env: bool,
    /// Where the main agent's calls run as jobs; without it `shell` is rig's, unchanged.
    pub jobs: Option<Arc<Jobs>>,
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

    /// Starts the command `args` names in the workspace, as rig's `shell` does, its stdout and
    /// stderr in pipes of their own.
    pub(crate) fn launch(&self, args: ShellArgs) -> Result<Process, ToolExecutionError> {
        let (shell, args) = self.prepared(args);
        let base = shell.launcher.cwd.as_deref();
        let dir = match &args.cwd {
            Some(dir) => {
                if shell.confine {
                    shell.workspace.place(base, true, dir)?;
                }
                Some(
                    shell
                        .workspace
                        .join(base.unwrap_or(Path::new("")), Path::new(dir)),
                )
            }
            None => base.map(Path::to_path_buf),
        };
        let how = Spawn {
            stdin: None,
            merge_stderr: false,
        };
        let line = [OsString::from("-c"), OsString::from(&args.command)];
        shell
            .workspace
            .spawn(
                &shell.launcher,
                dir.as_deref(),
                OsStr::new("sh"),
                &line,
                how,
            )
            .map_err(|source| ToolExecutionError::other(chain(&JobError::Spawn { source })))
    }

    async fn call_job(
        &self,
        context: &mut ToolContext,
        jobs: &Jobs,
        call: ShellCall,
    ) -> Result<String, ToolExecutionError> {
        let max = self.shell.max_timeout.max(1);
        let timeout = Duration::from_secs(
            call.timeout_secs
                .unwrap_or(DEFAULT_TIMEOUT.min(self.shell.max_timeout))
                .clamp(1, max),
        );
        let quiet = quiet_for(call.timeout_secs, timeout);
        let background = call.run_in_background.unwrap_or(false);
        if background && !jobs.has_room() {
            return Err(job_error(&JobError::TooMany));
        }
        let no_promote = context
            .get::<NoPromote>()
            .ok()
            .flatten()
            .is_some_and(|flag| flag.0);
        let process = self.launch(call.shell_args())?;
        let id = jobs
            .start(
                Start {
                    command: call.command,
                    background,
                    monitor: None,
                },
                process,
            )
            .map_err(|e| job_error(&e))?;
        if background {
            return Ok(format!(
                "[started in the background as {id}. You are told when it ends; use job_output to read its output, job_cancel to stop it.]"
            ));
        }
        let ran = jobs.foreground(&id, timeout, quiet, !no_promote).await;
        let limit = self.shell.cut.unwrap_or_default();
        let (mut text, fact) = cut::cut(&ran.chunk.lines, ran.chunk.missed, limit);
        let silent = ran.chunk.lines.is_empty();
        let status = match &ran.end {
            None => {
                let why = match (ran.quiet, quiet) {
                    (true, Some(quiet)) => format!("printed nothing for {}s", quiet.as_secs()),
                    _ => format!("still running after {}s", timeout.as_secs()),
                };
                text.push_str(&format!(
                    "[{why}; moved to background job {id}. Its output so far is above. Use \
                     job_output to read more or wait for it, job_cancel to stop it. You are \
                     told when it ends.]"
                ));
                None
            }
            Some(info) => {
                let stopped = cut::stopped(info.forced, info.remote.as_deref());
                let (line, status) = match info.end {
                    End::Exited(status) => cut::exit_line(status),
                    End::Killed(Kill::Timeout) => {
                        cut::timed_out_line(timeout.as_secs(), max, &stopped, silent)
                    }
                    End::Killed(Kill::Quiet) => {
                        cut::quiet_line(quiet.unwrap_or_default().as_secs(), &stopped, silent)
                    }
                    End::Killed(Kill::Cap) => (
                        format!(
                            "[stopped: {MAX_RUNNING} jobs are already running, so it could not \
                             move to the background; {stopped}. Stop one with job_cancel and \
                             run it again]"
                        ),
                        ExitStatus::Killed,
                    ),
                    End::Runaway => cut::runaway_line(OUTPUT_CAP / 1024 / 1024, &stopped),
                    End::Killed(_) => (
                        format!("[terminated before it ended: {stopped}]"),
                        ExitStatus::Killed,
                    ),
                    End::Lost => {
                        return Err(ToolExecutionError::other("waiting for the command failed"));
                    }
                };
                text.push_str(&line);
                Some(status)
            }
        };
        if let Some(status) = status {
            context.insert_result(status)?;
        }
        if let Some(fact) = fact {
            context.insert_result(fact)?;
        }
        Ok(text)
    }
}

/// The output a subagent's shell call may print before rig kills it.
const SUBAGENT_OUTPUT_MB: u64 = 10;

impl Tool for ShellTool {
    const NAME: &'static str = Shell::NAME;
    type Args = ShellCall;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        let max = self.shell.max_timeout.max(1);
        let kept = match self.shell.cut {
            Some(cut) => format!(
                "; past {} lines or {} KB, its first and last lines and the lines with errors \
                 or failures from between",
                cut.lines,
                cut.bytes / 1024
            ),
            None => String::new(),
        };
        format!(
            "Run a shell command with `sh -c` in the current directory, or in cwd. stdin is \
             empty. Returns the exit code and stdout and stderr interleaved line by line{kept}. \
             A command still running at timeout_secs (default {}, at most {max}), or, with the \
             default, once it printed nothing for {}s, keeps running as a background job and \
             you are told when it ends; pass run_in_background for a server or a long build you \
             do not need to wait for, and keep its process in the foreground of the command \
             (`npm start`, not `npm start &`). Do not poll with `sleep`: use job_output with \
             wait_secs. Background jobs are for the main agent only: a subagent's command is \
             stopped at its timeout and run_in_background is refused. A command that prints \
             more than {} MB is killed ({} MB in a subagent). Use it to build, test, \
             run programs and use git. Each call starts a fresh `sh`: a `cd` or a variable does \
             not carry over to the next call. Do not start programs that wait for input, such \
             as editors or pagers. Do not assume a git repository: a failed `git` command ends \
             a `&&` chain. Check formatting only of files you changed.",
            DEFAULT_TIMEOUT.min(self.shell.max_timeout).max(1),
            QUIET_LIMIT.as_secs(),
            OUTPUT_CAP / 1024 / 1024,
            SUBAGENT_OUTPUT_MB,
        )
    }

    fn parameters(&self) -> serde_json::Value {
        let mut schema = self.shell.parameters();
        if let Some(properties) = schema
            .get_mut("properties")
            .and_then(serde_json::Value::as_object_mut)
        {
            properties.insert(
                "run_in_background".to_owned(),
                json!({
                    "type": ["boolean", "null"],
                    "description": "Main agent only. Start the command as a background job and \
                                    return at once; you are told when it ends.",
                }),
            );
        }
        schema
    }

    fn annotations(&self) -> ToolAnnotations {
        self.shell.annotations()
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: ShellCall,
    ) -> Result<String, ToolExecutionError> {
        let agent = context
            .get::<AgentId>()
            .ok()
            .flatten()
            .map_or_else(|| ROOT_AGENT.to_string(), |id| id.0);
        match (&self.jobs, agent == ROOT_AGENT) {
            (Some(jobs), true) => {
                let jobs = jobs.clone();
                self.call_job(context, &jobs, args).await
            }
            _ => {
                if args.run_in_background == Some(true) {
                    return Err(job_error(&JobError::NotRoot));
                }
                let (shell, shell_args) = self.prepared(args.shell_args());
                shell.call(context, shell_args).await
            }
        }
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

pub fn notebook_edit_for(root: &Path, seen: &Seen) -> NotebookEdit {
    NotebookEdit {
        base: Some(root.to_path_buf()),
        confine: true,
        seen: seen.clone(),
        ..NotebookEdit::default()
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
        jobs: None,
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
    /// The session's jobs. Passing the same one again when the registry is rebuilt keeps the
    /// running ones.
    pub jobs: Arc<Jobs>,
    /// How `job_output` waits on a subagent; with none, agent ids are refused.
    pub agent_waits: Option<Arc<dyn AgentWaits>>,
    /// What ends a `job_output` wait early.
    pub wait_wake: Option<Arc<dyn WaitWake>>,
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
            |root| NotebookRead {
                read: read_for(root, &seen),
            },
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
            |root| WriteTool {
                write: write_for(root, &seen),
            },
        )),
        register(GuardedTool::per_root(
            scope.clone(),
            risk_of(Edit::NAME),
            guard.clone(),
            |root| EditTool {
                edit: edit_for(root, &seen),
            },
        )),
        register(GuardedTool::per_root(
            scope.clone(),
            risk_of(NotebookEdit::NAME),
            guard.clone(),
            |root| notebook_edit_for(root, &seen),
        )),
        register(GuardedTool::per_root(
            scope.clone(),
            risk_of(ShellTool::NAME),
            guard.clone(),
            |root| ShellTool {
                jobs: Some(env.jobs.clone()),
                ..shell_for(root, &env.shell)
            },
        )),
        register(GuardedTool::single(
            JobOutputTool {
                jobs: env.jobs.clone(),
                limit: OutputLimit::default(),
                agents: env.agent_waits.clone(),
                wake: env.wait_wake.clone(),
            },
            risk_of(JobOutputTool::NAME),
            guard.clone(),
        )),
        register(GuardedTool::single(
            JobCancelTool {
                jobs: env.jobs.clone(),
                limit: OutputLimit::default(),
            },
            risk_of(JobCancelTool::NAME),
            guard.clone(),
        )),
        register(GuardedTool::single(
            MonitorTool {
                shell: ShellTool {
                    jobs: Some(env.jobs.clone()),
                    ..shell_for(scope.primary(), &env.shell)
                },
            },
            risk_of(MonitorTool::NAME),
            guard.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_default_timeout_has_a_quiet_limit() {
        let two_minutes = Duration::from_secs(120);
        assert_eq!(quiet_for(None, two_minutes), Some(QUIET_LIMIT));
        assert_eq!(quiet_for(Some(120), two_minutes), None);
        assert_eq!(quiet_for(None, Duration::from_secs(30)), None);
    }

    #[test]
    fn the_model_cannot_pass_an_argument_the_tool_does_not_define() {
        let parsed: Result<ShellCall, _> = serde_json::from_value(
            json!({"command": "ls", "run_in_background": true, "max_timeout": 0}),
        );
        assert!(parsed.is_err());
    }
}
