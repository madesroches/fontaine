//! The agent of a session: the wrapped tool registry, the permission pipeline, the model and
//! its memory, assembled the same way for the headless surface and the TUI. `/add-dir` rebuilds
//! the registry and the agent through the same path between turns.

#![deny(clippy::too_many_lines)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use fontaine_agents::registry::AgentTypes;
use fontaine_agents::tree::AgentTree;
use fontaine_guardian::Profile;
use fontaine_mcp::Connected;
use fontaine_tools::builtins::Seen;
use fontaine_tools::error_chain::chain;
use fontaine_tools::gate::{CallView, ROOT_AGENT};
use fontaine_tools::git_bash::GitBash;
use fontaine_tools::jobs::Jobs;
use fontaine_tools::lock::write_recovering;
use fontaine_tools::lsp::LspServers;
use fontaine_tools::powershell::PowerShellSettings;
use fontaine_tools::result_pipeline::ResultPipeline;
use fontaine_tools::risk::RiskClass;
use fontaine_tools::websearch::SearxngChild;
use rig_agent::prelude::*;
use rig_agent::tool::server::ToolServerHandle;
use rig_core::completion::ToolDefinition;
use rig_core::schemars::Schema;
use rig_core::tool::ToolOutput;
use rig_memory::{estimate_tokens, load_async};
use serde_json::json;

use crate::child_runner::{CoreChildRunner, MainModel};
use crate::config::{Config, PermissionMode};
use crate::context::{self, ContextError, ContextUsage};
use crate::extensions::Extensions;
use crate::gate::SessionState;
use crate::instructions::{self, InstructionStage, Loaded, MemoryEntry};
use crate::ledger::{FILE_INDEX_CAP, FileIndex, Record, git_remotes_of};
use crate::memory;
use crate::models::{AccountListing, Models, SharedSwitcher, Switcher};
use crate::native_search;
use crate::notice::Notice;
use crate::permissions::{Approver, PermissionPipeline};
use crate::project_env::{self, ProjectEnvironment};
use crate::prompt;
use crate::runtime_error::RuntimeError;
use crate::scanner_stage::{Backend, ScannerStart};
use crate::schedule::{Fired, Kind, Origin, SELF_PACED_NOTE};
use crate::session::{CONVERSATION_LOG, ClearError, Session};
use crate::skills::SkillRuntime;
use crate::steering::Steering;
use crate::tools;
use crate::trust;
use crate::workspace::{self, RootSource};

mod build;
mod goal;
mod lsp;
mod mcp;
mod messaging;
mod models;
mod rewind;
mod setup;

pub(crate) use build::apply_common;
use build::{Parts, build_agent};
pub use mcp::{McpOutcome, McpServerPlan};
pub use messaging::{Delivered, MessageRefusal};
pub use models::{CredentialEffect, Switched};
pub use rewind::{RewindError, RewindOutcome};
pub use setup::{build_agent_types, build_guardian, build_guardian_telling, build_screen};

pub type EnvLookup = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

/// How the session began, for the ledger and the surfaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionOrigin {
    Fresh,
    /// Reopened (`resume`, `continue`) or copied (`fork`) from the session `source`.
    Continued {
        source: String,
        how: String,
        /// Whether starting the session appends the `Resumed` record. Off for a copy whose
        /// `Resumed` was written when it was forked.
        record_resume: bool,
    },
}

/// Everything a session needs that does not depend on which surface runs it.
pub struct AgentSetup {
    pub config: Config,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub models: Models,
    pub session: Session,
    /// The workspace roots, primary first, canonical.
    pub roots: Vec<PathBuf>,
    /// The session's scratch root, which is also the last of `roots`.
    pub scratch: Option<PathBuf>,
    /// The auto-memory store, also one of `roots`, when `[memory] auto` is on and it could be
    /// created.
    pub memory_root: Option<PathBuf>,
    pub mode: PermissionMode,
    /// Variables the shell tool's child processes do not inherit.
    pub scrub_env: Vec<OsString>,
    pub git_bash: Option<GitBash>,
    /// The `powershell` tool's interpreter, found on Windows; `None` leaves the tool out.
    pub powershell: Option<PowerShellSettings>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    /// A schema the final answer must follow.
    pub json_schema: Option<Schema>,
    /// Nothing can be asked: every would-prompt outcome is a denial.
    pub headless: bool,
    /// Whether this surface runs scheduled prompts when the conversation is idle, so the
    /// scheduling tools are worth registering. A run that exits after its answer leaves it off.
    pub scheduling: bool,
    pub approver: Arc<dyn Approver>,
    pub home: Option<PathBuf>,
    /// The user settings file "always allow" appends to, and `/model` and `/guardian-model`
    /// write to.
    pub user_config: Option<PathBuf>,
    /// The guardian profile that is actually in force, after the containment check.
    pub guardian_profile: Profile,
    /// What to tell the user once at startup.
    pub notices: Vec<Notice>,
    /// Warnings gathered before the session started (config roots, the screen-reader setting),
    /// shown ahead of every other notice. Surfaces that print them elsewhere leave this empty.
    pub startup_warnings: Vec<Notice>,
    /// The tool-result scanner as started, and what it has to tell the user.
    pub scanner: ScannerStart,
    /// Resolves models again for `/model` and `/guardian-model`.
    pub switcher: Option<Switcher>,
    /// The skills, agent files and output styles that were loaded.
    pub extensions: Extensions,
    /// Whether project-defined executables (hooks, MCP servers, skill commands) may start. False
    /// when the user declined to trust the workspace.
    pub project_executables: bool,
    /// Whether each repository-defined MCP server needs the user's approval before it starts
    /// (the TUI); a headless run starts them without asking.
    pub confirm_project_servers: bool,
    pub origin: SessionOrigin,
    /// The process environment, for the variables MCP servers are given by name.
    pub env: EnvLookup,
    /// Whether to say that web search is unavailable. A `/resume` or `/branch` builds another
    /// runtime in the same process, so the launcher turns this off after the first.
    pub warn_no_search: bool,
}

pub struct AgentRuntime {
    pub agent: Agent,
    prompt_pieces: prompt::PromptPieces,
    launch_instructions: Loaded,
    instruction_stage: Arc<InstructionStage>,
    pub state: Arc<SessionState>,
    pub session: Session,
    pub models: Models,
    pub config: Config,
    /// The mode the session opened in; the live mode is the state's.
    start_mode: PermissionMode,
    guardian_profile: Profile,
    notices: Vec<Notice>,
    startup_warnings: Vec<Notice>,
    /// Scanner startup notices. Kept apart from `notices` because every surface shows them in
    /// every permission mode.
    scanner_notices: Vec<Notice>,
    origin: SessionOrigin,
    /// Messages typed while a turn runs, delivered with the next tool result.
    pub steering: Steering,
    /// The workspace files `@` completion offers: the session-start index, rebuilt by
    /// [`AgentRuntime::refresh_files`].
    pub files: Arc<RwLock<Arc<Vec<String>>>>,
    pipeline: Arc<PermissionPipeline>,
    approver: Arc<dyn Approver>,
    seen: Seen,
    pub(crate) setup: Setup,
    tool_server: ToolServerHandle,
    results: Arc<ResultPipeline>,
    /// The `llm` scanner's slot, which `install_guardian` keeps on the guardian's model.
    llm_scanner: Option<Arc<RwLock<Backend>>>,
    skills: Arc<SkillRuntime>,
    agent_types: Arc<AgentTypes>,
    extensions: Arc<Extensions>,
    main: Arc<RwLock<MainModel>>,
    runner: Arc<CoreChildRunner>,
    output_style: Option<String>,
    mcp: Vec<Connected>,
    /// What `model_listings` has fetched this session.
    listings: Vec<AccountListing>,
    /// Whether the agent was built with the provider's search tool.
    native_search: bool,
}

/// The parts of the setup a rebuild needs again.
pub(crate) struct Setup {
    /// The global `[network]` settings, loaded once; accounts resolve theirs on demand.
    network: Arc<fontaine_tools::tls::LoadedNetwork>,
    config_dir: PathBuf,
    home: Option<PathBuf>,
    data_dir: PathBuf,
    pub(crate) scrub_env: Vec<OsString>,
    pub(crate) git_bash: Option<GitBash>,
    pub(crate) powershell: Option<PowerShellSettings>,
    system_prompt: Option<String>,
    append_system_prompt: Option<String>,
    json_schema: Option<Schema>,
    user_config: Option<PathBuf>,
    switcher: Option<SharedSwitcher>,
    project_executables: bool,
    confirm_project_servers: bool,
    scheduling: bool,
    env: EnvLookup,
    /// The SearXNG process `[websearch] command` starts, created once so a registry rebuild
    /// reuses it. Dropping the runtime stops it.
    searxng: Arc<SearxngChild>,
    /// The session's background jobs, created once so a registry rebuild keeps the running
    /// ones. Dropping the runtime terminates them.
    pub(crate) jobs: Arc<Jobs>,
    /// The language servers, created once so a registry rebuild keeps the running ones. They
    /// end with `/clear`, `/rewind` and the runtime.
    pub(crate) lsp: Arc<LspServers>,
}

/// One line when the main model has no way to search: no provider search for its account, no
/// configured backend and no container runtime for the default one.
fn warn_if_no_search(setup: &AgentSetup) {
    if setup.warn_no_search
        && !native_search::provider_available(&setup.config, &setup.models)
        && setup.config.websearch.backend.is_none()
        && setup.config.default_search_runtime(&*setup.env).is_none()
    {
        setup.approver.warn(
            "web search is unavailable: install docker or podman, or set `[websearch] backend`",
        );
    }
}

impl AgentRuntime {
    pub fn new(mut setup: AgentSetup) -> Result<AgentRuntime, RuntimeError> {
        setup::prepare(&mut setup);
        let tree = Arc::new(AgentTree::new(setup::agent_limits(&setup.config)));
        let agent_types = Arc::new(build_agent_types(&setup.config, &setup.extensions));
        let state = setup::session_state(&setup, &tree, &agent_types);
        setup::attach_screen(&state, &setup);
        let setup::Gates {
            pipeline,
            retained,
            instruction_stage,
            results,
            llm_scanner,
        } = setup::gates(&mut setup, &tree, &state)?;
        let (extensions, skills) =
            setup::extension_runtimes(setup.extensions, &state, setup.project_executables);
        let steering = Steering::default();
        let (main, tool_server, runner) = setup::subagent_support(
            &retained,
            &tree,
            &state,
            &setup.models,
            &setup.config,
            &setup.approver,
            &steering,
            &results,
        );
        let seen = Seen::default();
        let (agent, pieces, loaded, native) = build_agent(&Parts {
            setup: &retained,
            roots: state.scope().roots(),
            pipeline: &pipeline,
            state: &state,
            seen: &seen,
            models: &setup.models,
            config: &setup.config,
            session: &setup.session,
            tool_server: &tool_server,
            results: &results,
            skills: &skills,
            approver: &setup.approver,
            types: &agent_types,
            runner: &runner,
            output_style: None,
            steering: &steering,
            instructions: &instruction_stage,
        })?;
        Ok(AgentRuntime {
            agent,
            prompt_pieces: pieces,
            launch_instructions: loaded,
            instruction_stage,
            state,
            session: setup.session,
            models: setup.models,
            config: setup.config,
            start_mode: setup.mode,
            guardian_profile: setup.guardian_profile,
            notices: setup.notices,
            startup_warnings: setup.startup_warnings,
            scanner_notices: setup.scanner.notices,
            origin: setup.origin,
            steering,
            files: Arc::default(),
            pipeline,
            approver: setup.approver,
            seen,
            setup: retained,
            tool_server,
            results,
            llm_scanner,
            skills,
            agent_types,
            extensions,
            main,
            runner,
            output_style: None,
            mcp: Vec::new(),
            listings: Vec::new(),
            native_search: native,
        })
    }

    /// Rebuilds the registry and the agent for the current scope. The conversation lives in the
    /// session store, so a new agent continues it.
    fn rebuild(&mut self) -> Result<(), RuntimeError> {
        let scope = self.state.scope();
        let (agent, prompt_pieces, launch_instructions, native_search) = build_agent(&Parts {
            setup: &self.setup,
            roots: scope.roots(),
            pipeline: &self.pipeline,
            state: &self.state,
            seen: &self.seen,
            models: &self.models,
            config: &self.config,
            session: &self.session,
            tool_server: &self.tool_server,
            results: &self.results,
            skills: &self.skills,
            approver: &self.approver,
            types: &self.agent_types,
            runner: &self.runner,
            output_style: self.output_style_text(),
            steering: &self.steering,
            instructions: &self.instruction_stage,
        })?;
        self.native_search = native_search;
        self.agent = agent;
        self.prompt_pieces = prompt_pieces;
        self.launch_instructions = launch_instructions;
        Ok(())
    }

    fn native_search_entry(&self) -> Option<serde_json::Value> {
        native_search::offered(&self.config, &self.models, &self.state)
    }

    /// Rebuilds the agent when the live mode and rules change whether the provider's search
    /// tool is offered. The tool is part of the built agent, which a mode switch or a session
    /// rule does not rebuild, so the turn drivers call this before each turn.
    ///
    /// A failed rebuild is an error only when the old agent would keep a search tool the gate now
    /// withholds, so the caller must not run the turn; failing to newly offer it is warned about
    /// and the turn runs without it.
    pub fn refresh_native_search(&mut self) -> Result<(), RuntimeError> {
        let offered = self.native_search_entry().is_some();
        if offered == self.native_search {
            return Ok(());
        }
        match self.rebuild() {
            Err(e) if !offered => Err(e),
            Err(e) => {
                self.approver.warn(&format!(
                    "the provider's web search could not be updated: {}",
                    fontaine_tools::error_chain::chain(&e)
                ));
                Ok(())
            }
            Ok(()) => Ok(()),
        }
    }

    /// Whether the agent carries the provider's search tool now.
    pub fn native_search_active(&self) -> bool {
        self.native_search
    }

    fn output_style_text(&self) -> Option<(String, String)> {
        let name = self.output_style.as_deref()?;
        let style = self.extensions.output_style(name)?;
        Some((style.name.clone(), style.body.clone()))
    }

    /// What `/memory` lists: the instruction files in the prompt, the ones read on demand, and
    /// the `FONTAINE.md` files that can be created.
    pub fn memory_entries(&self) -> Vec<MemoryEntry> {
        instructions::memory_entries(
            &self.launch_instructions,
            &self.instruction_stage.loaded_on_demand(),
            &self.setup.config_dir,
            &self.primary_root(),
            Some(&self.memory_dir()),
        )
    }

    /// Where this project's auto-memory notes live, whether or not auto memory is on.
    pub fn memory_dir(&self) -> PathBuf {
        memory::dir(&self.setup.data_dir, &self.primary_root())
    }

    /// Whether this session saves and loads auto-memory notes.
    pub fn auto_memory_active(&self) -> bool {
        self.state.memory_root.is_some()
    }

    /// The names of the tools the agent has now, MCP tools included.
    pub fn tool_names(&self) -> Vec<String> {
        self.tool_server
            .toolset()
            .names()
            .map(str::to_owned)
            .collect()
    }

    /// The tool a call named `agent` would be served by, for the surfaces' tests.
    pub fn tool_server(&self) -> &ToolServerHandle {
        &self.tool_server
    }

    pub fn extensions(&self) -> &Arc<Extensions> {
        &self.extensions
    }

    /// The user config directory, for reports that read the plugin registry.
    pub fn config_dir(&self) -> &Path {
        &self.setup.config_dir
    }

    pub fn agent_types(&self) -> &Arc<AgentTypes> {
        &self.agent_types
    }

    pub fn skills(&self) -> &Arc<SkillRuntime> {
        &self.skills
    }

    /// The active output style, if any.
    pub fn output_style(&self) -> Option<&str> {
        self.output_style.as_deref()
    }

    /// `/output-style`: makes `name` the style for the next turn, or none. The style only
    /// changes the agent's prompt; the guardian's request is the same with or without it.
    pub fn set_output_style(&mut self, name: Option<&str>) -> Result<(), RuntimeError> {
        if let Some(name) = name
            && self.extensions.output_style(name).is_none()
        {
            let known: Vec<&str> = self
                .extensions
                .output_styles
                .iter()
                .map(|s| s.name.as_str())
                .collect();
            return Err(RuntimeError::new(format!(
                "no output style named `{name}`; available: {}",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            )));
        }
        let previous = std::mem::replace(&mut self.output_style, name.map(str::to_string));
        if let Err(e) = self.rebuild() {
            self.output_style = previous;
            return Err(e);
        }
        Ok(())
    }

    /// The skills a user can run as `/name`: name, argument hint and description.
    pub fn slash_skills(&self) -> Vec<(String, Option<String>, String)> {
        self.extensions
            .skills
            .iter()
            .filter(|skill| skill.user_invocable)
            .map(|skill| {
                (
                    skill.name.clone(),
                    skill.argument_hint.clone(),
                    skill.description.clone(),
                )
            })
            .collect()
    }

    /// `/name arguments`: the skill's text for the root agent, ready to be the next prompt.
    pub async fn expand_skill(&self, name: &str, arguments: &str) -> Result<String, RuntimeError> {
        // A user-typed skill is no model call, so its commands must not inherit the previous
        // turn's tool_use_id.
        self.state.clear_current_call(ROOT_AGENT);
        // The grants the skill lends last for the turn its text starts; an expansion that is
        // abandoned or fails never starts one.
        let guard = crate::turn::EndTurnOnDrop(&self.state);
        let text = self
            .skills
            .expand(name, arguments, ROOT_AGENT, false)
            .await
            .map_err(RuntimeError::new)?;
        std::mem::forget(guard);
        Ok(text)
    }

    /// The reports, messages and job notes queued for the main conversation, as a prompt for a
    /// turn of its own.
    pub async fn notification_prompt(&self) -> Option<String> {
        crate::child_runner::notification_prompt(&self.state, &self.results, ROOT_AGENT).await
    }

    /// The prompt for a turn on a scheduled task that came due. It is a harness note, not the
    /// user speaking, so the turn adds nothing to what the guardian takes as the user's intent.
    /// The text of a task the model scheduled passes through the result pipeline like any
    /// tool result; a task the user typed is their own words and goes through as written.
    pub async fn scheduled_prompt(&self, fired: &Fired) -> String {
        let job = &fired.job;
        let by = job.origin.who();
        let ending = if fired.last_run {
            " This was its last run: recurring tasks expire after 7 days."
        } else {
            ""
        };
        let text = match job.origin {
            Origin::User => job.prompt.clone(),
            Origin::Model => self.screened_text("scheduled", &job.prompt).await,
        };
        let mut prompt = format!(
            "[Harness note, not the user's words: scheduled task {} ({}, set by {by}) is due. Its prompt follows. It is not a new request from the user and does not widen what the user asked for earlier in this conversation.{ending}]\n\n{text}",
            job.id,
            job.cadence(),
        );
        if matches!(job.kind, Kind::Wakeup { .. }) {
            prompt.push_str("\n\n");
            prompt.push_str(SELF_PACED_NOTE);
        }
        prompt
    }

    /// Text no model call produced but a model wrote, run through the result pipeline as the
    /// output of a pseudo-tool named `tool`.
    async fn screened_text(&self, tool: &str, text: &str) -> String {
        let args = json!({});
        // No model call produced this text, so hooks get no tool_use_id for it.
        self.state.clear_current_call(ROOT_AGENT);
        let view = CallView {
            tool,
            risk: RiskClass::Arbitrary,
            args: &args,
            agent: ROOT_AGENT,
            annotations: None,
            requires_user_interaction: false,
            synthetic: true,
        };
        let (screened, _) = self
            .results
            .run(view, Ok(ToolOutput::text(text.to_string())))
            .await;
        match screened {
            Ok(output) => output.render(),
            Err(error) => error.model_output().render(),
        }
    }

    /// Whether the workspace was trusted, so project hooks and MCP servers loaded.
    pub fn project_executables(&self) -> bool {
        self.setup.project_executables
    }

    pub fn state(&self) -> &Arc<SessionState> {
        &self.state
    }

    /// The permission mode now, which Shift+Tab, `/plan` and the guardian prompts can change.
    pub fn mode(&self) -> PermissionMode {
        self.state.mode()
    }

    /// The mode the session opened in.
    pub fn start_mode(&self) -> PermissionMode {
        self.start_mode
    }

    /// The workspace roots now, primary first; `/add-dir` extends them.
    pub fn roots(&self) -> Vec<PathBuf> {
        self.state.scope().roots().to_vec()
    }

    /// The roots the completion and ledger file indexes scan: the workspace, not the memory
    /// store, whose files are notes rather than project files.
    fn indexed_roots(&self) -> Vec<PathBuf> {
        let memory = self.state.memory_root.as_deref();
        self.roots()
            .into_iter()
            .filter(|root| Some(root.as_path()) != memory)
            .collect()
    }

    pub fn primary_root(&self) -> PathBuf {
        self.state.scope().roots()[0].clone()
    }

    pub fn guardian_profile(&self) -> Profile {
        self.guardian_profile
    }

    pub fn origin(&self) -> &SessionOrigin {
        &self.origin
    }

    pub fn notices(&self) -> &[Notice] {
        &self.notices
    }

    pub fn startup_warnings(&self) -> &[Notice] {
        &self.startup_warnings
    }

    pub fn scanner_notices(&self) -> &[Notice] {
        &self.scanner_notices
    }

    /// Adds a workspace root from `/add-dir`: validated and canonicalized like `--add-dir`, then
    /// the scope, registry and agent are rebuilt for the next turn. The conversation and the
    /// session store are kept. Returns the canonical root.
    pub fn add_dir(&mut self, entry: &Path, launch_dir: &Path) -> Result<PathBuf, RuntimeError> {
        let resolved = workspace::resolve(
            &self.primary_root(),
            launch_dir,
            [(RootSource::Command, entry.to_path_buf())],
        );
        if let Some(warning) = resolved.warnings.first() {
            return Err(RuntimeError::new(warning.clone()));
        }
        let root = resolved
            .roots
            .into_iter()
            .nth(1)
            .ok_or_else(|| RuntimeError::new("that is the primary workspace root"))?;
        if self.roots().contains(&root) {
            return Err(RuntimeError::new(format!(
                "{} is already a workspace root",
                root.display()
            )));
        }
        let previous_scope = self.state.scope();
        self.state
            .set_scope(Arc::new(previous_scope.with_root(root.clone())));
        if let Err(e) = self.rebuild() {
            self.state.set_scope(previous_scope);
            return Err(e);
        }
        self.setup.lsp.root_added(root.clone());
        self.state.append(Record::AddDir {
            root: root.display().to_string(),
        });
        Ok(root)
    }

    /// The facts the ledger opens with: the roots, trust, git remotes, the names of the `[env]`
    /// overrides and the pre-session file index.
    pub async fn record_session_start(&self) {
        // A continued session keeps the file index it opened with: what it created since is
        // known to the ledger, and a fresh index would call those files pre-existing.
        let continued = matches!(self.origin, SessionOrigin::Continued { .. });
        let roots = self.roots();
        let scanned = self.indexed_roots();
        let (data_dir, primary) = (self.setup.data_dir.clone(), self.primary_root());
        let (index, git_remotes, stored) = tokio::task::spawn_blocking(move || {
            let index = FileIndex::build(&scanned, FILE_INDEX_CAP);
            let remotes = git_remotes_of(&scanned);
            // File I/O only: setup is an explicit command, never a step of session start.
            let stored = project_env::load(&data_dir, &primary);
            (index, remotes, stored)
        })
        .await
        .unwrap_or_default();
        let (project_environment, setup_notice) =
            self.project_environment_at_start(stored, &git_remotes);
        self.set_files(index.paths.clone());
        if let Some(message) = trust::unreadable(&self.setup.data_dir) {
            self.approver.warn(&message);
        }
        let trusted = trust::is_trusted(&self.setup.data_dir, &self.config.workspace);
        self.append(Record::SessionStart {
            session_id: self.session.id.clone(),
            permission_mode: self.start_mode.as_str().to_string(),
            workspace_roots: roots
                .iter()
                .map(|root| root.display().to_string())
                .collect(),
            workspace_trust: if trusted { "trusted" } else { "untrusted" }.to_string(),
            git_remotes,
            env_keys: tools::subprocess_env(&self.config)
                .into_iter()
                .map(|(name, _)| name.to_string_lossy().into_owned())
                .collect(),
            project_environment,
        });
        if let Some(notice) = setup_notice {
            self.approver.warn(&notice.to_string());
            self.state.mark_setup_warning_shown();
        }
        if let SessionOrigin::Continued {
            source,
            how,
            record_resume: true,
        } = &self.origin
        {
            self.append(Record::Resumed {
                source: source.clone(),
                how: how.clone(),
            });
        }
        if !continued {
            self.append(Record::FileIndex {
                paths: index.paths,
                truncated: index.truncated,
            });
        }
    }

    /// The stored project facts as this session uses them, and the warning to raise when they
    /// are missing or stale in an `auto` session.
    fn project_environment_at_start(
        &self,
        stored: project_env::Loaded,
        git_remotes: &[String],
    ) -> (Option<ProjectEnvironment>, Option<Notice>) {
        if let Some(warning) = &stored.warning {
            self.approver.warn(warning);
        }
        let now = chrono::Utc::now();
        let record = stored
            .record
            .map(|record| project_env::filter_to_remotes(record, git_remotes));
        let notice = project_env::setup_notice(
            record.as_ref(),
            git_remotes,
            self.state.mode(),
            &self.config.auto_mode,
            now,
        );
        (
            record.map(|record| project_env::expire(record, now)),
            notice,
        )
    }

    /// The warning for the first switch into `auto` of a session that did not start there, or
    /// None when the human has been told already, the guardian is off, or nothing is missing.
    pub fn setup_notice_on_auto(&self) -> Option<Notice> {
        if self.state.setup_warning_shown() || self.state.guardian_off() {
            return None;
        }
        let remotes = self.state.ledger.view().git_remotes.clone();
        let stored = project_env::load(&self.setup.data_dir, &self.primary_root());
        let record = stored
            .record
            .map(|record| project_env::filter_to_remotes(record, &remotes));
        let notice = project_env::setup_notice(
            record.as_ref(),
            &remotes,
            self.state.mode(),
            &self.config.auto_mode,
            chrono::Utc::now(),
        )?;
        self.state.mark_setup_warning_shown();
        Some(notice)
    }

    /// The git remotes of every indexed root, as session start records them.
    pub fn session_remotes(&self) -> Vec<String> {
        git_remotes_of(&self.indexed_roots())
    }

    /// Stores the record `/auto-mode-setup` produced exactly as the command line does, then
    /// applies it to this session from the next guardian call on.
    pub fn apply_project_environment(&self, record: ProjectEnvironment) -> std::io::Result<()> {
        project_env::store(&self.setup.data_dir, &self.primary_root(), &record)?;
        self.state.apply_project_environment(record);
        Ok(())
    }

    fn set_files(&self, paths: Vec<String>) {
        // The completion index is advisory: a stale list only misses a suggestion.
        *write_recovering(&self.files) = Arc::new(paths);
    }

    /// Rebuilds the completion file list in the background, so files the agent created and
    /// directories added since start become completable. Never blocks the caller.
    pub fn refresh_files(&self) {
        let roots = self.indexed_roots();
        let slot = self.files.clone();
        tokio::task::spawn_blocking(move || {
            let index = FileIndex::build(&roots, FILE_INDEX_CAP);
            // The completion index is advisory: a stale list only misses a suggestion.
            *write_recovering(&slot) = Arc::new(index.paths);
        });
    }

    /// The session's approver, for warnings from work done outside the permission pipeline.
    pub fn approver(&self) -> &dyn Approver {
        self.approver.as_ref()
    }

    pub fn append(&self, record: Record) {
        self.state.append(record);
    }

    /// `/clear`: empties the model's context in this session. Refused while a subagent runs,
    /// because its guardian requests read the human messages the clear drops, and while a
    /// finished subagent's report is undelivered, which would otherwise reach the empty context.
    pub async fn clear_conversation(&self) -> Result<(), ClearError> {
        let agents = self.state.agents();
        if agents.any_running() || agents.has_agent_notifications() {
            return Err(ClearError::SubagentsRunning);
        }
        self.session.clear().await?;
        self.end_jobs().await;
        self.state.append(Record::ContextCleared {});
        if let Err(e) = self.session.todos().clear() {
            self.approver
                .warn(&format!("the task list was not cleared: {}", chain(&e)));
        }
        // A task the model planted under the intent this clear drops must not outlive it.
        if self.config.schedule.enabled
            && let Err(e) = self.session.scheduler().clear()
        {
            self.approver.warn(&format!(
                "the scheduled tasks were not cleared: {}",
                chain(&e)
            ));
        }
        // The goal was set under the intent this clear drops.
        self.clear_goal();
        Ok(())
    }

    /// Terminates the running background jobs and drops the notes they queued: they belong to
    /// the conversation that is about to be emptied.
    pub(crate) async fn end_jobs(&self) {
        self.setup.jobs.terminate_all().await;
        self.setup.lsp.shutdown_all().await;
        self.state.agents().discard_job_notifications();
    }

    /// The session's background jobs, for the surface that lists and kills them.
    pub fn jobs(&self) -> &Arc<Jobs> {
        &self.setup.jobs
    }

    /// The session's language servers, for the surface that stops them at exit.
    pub fn lsp(&self) -> &Arc<LspServers> {
        &self.setup.lsp
    }

    /// `/context`: what fills the window now, estimated.
    pub async fn context_usage(&self) -> Result<ContextUsage, ContextError> {
        let conversation = load_async(&self.session.store(), CONVERSATION_LOG)
            .await
            .map_err(ContextError::Conversation)?;
        let defs = self
            .tool_server
            .tool_defs(None)
            .await
            .map_err(|e| ContextError::Tools(Box::new(e)))?;
        let defs: Vec<ToolDefinition> = defs.iter().map(|def| (**def).clone()).collect();
        let (tools, mcp_tools) = context::tool_tokens(&defs);
        let pieces = &self.prompt_pieces;
        let spec = self.models.main.completion.spec();
        let count = |text: &Option<String>| text.as_deref().map_or(0, context::text_tokens);
        Ok(ContextUsage {
            model: self.models.main.reference.to_string(),
            window: spec.and_then(|spec| spec.context_window),
            autocompact_at: self.session.autocompact_at(spec),
            configured: self.config.context.autocompact_window,
            reserve: self.session.reserve(spec),
            listed: spec.is_some(),
            system_prompt: context::text_tokens(&pieces.base)
                + count(&pieces.append)
                + count(&pieces.scheduling),
            output_style: count(&pieces.output_style),
            instructions: pieces
                .instructions
                .iter()
                .map(|(path, text)| (path.clone(), context::text_tokens(text)))
                .collect(),
            memory: count(&pieces.memory),
            skills: count(&pieces.skills),
            tools,
            mcp_tools,
            messages: estimate_tokens(conversation.messages()),
        })
    }
}
