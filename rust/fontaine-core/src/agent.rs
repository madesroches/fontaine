//! The agent of a session: the wrapped tool registry, the permission pipeline, the model and
//! its memory, assembled the same way for the headless surface and the TUI. `/add-dir` rebuilds
//! the registry and the agent through the same path between turns.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fontaine_tools::builtins::Seen;
use fontaine_tools::gate::{AgentId, ROOT_AGENT};
use fontaine_tools::git_bash::GitBash;
use fontaine_tools::scope::Scope;
use rig_agent::agent::{AgentHook, DispatchAction, DispatchEvent, HookContext};
use rig_agent::prelude::*;
use rig_core::schemars::Schema;

use crate::config::{Config, PermissionMode};
use crate::gate::{SessionState, StateParams};
use crate::ledger::{FILE_INDEX_CAP, FileIndex, Record, git_remotes};
use crate::models::Models;
use crate::permissions::{Approver, PermissionPipeline, PipelineParams};
use crate::prompt::{self, PromptParts};
use crate::session::{CONVERSATION_LOG, Session};
use crate::tools::{self, RegistryParams};
use crate::trust;
use crate::workspace::{self, RootSource};

/// Everything a session needs that does not depend on which surface runs it.
pub struct AgentSetup {
    pub config: Config,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub models: Models,
    pub session: Session,
    /// The workspace roots, primary first, canonical.
    pub roots: Vec<PathBuf>,
    pub mode: PermissionMode,
    /// Variables the shell tool's child processes do not inherit.
    pub scrub_env: Vec<OsString>,
    pub git_bash: Option<GitBash>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    /// A schema the final answer must follow.
    pub json_schema: Option<Schema>,
    /// Nothing can be asked: every would-prompt outcome is a denial.
    pub headless: bool,
    pub approver: Arc<dyn Approver>,
    pub home: Option<PathBuf>,
    /// The user settings file "always allow" appends to.
    pub user_config: Option<PathBuf>,
}

/// Tells the permission state which model tool call a tool is serving.
struct CurrentCall {
    state: Arc<SessionState>,
}

impl AgentHook for CurrentCall {
    async fn on_dispatch(&self, _ctx: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        if event.tool_name().is_some()
            && let Some(call) = event.call_id
        {
            let agent = event
                .tool_context()
                .and_then(|context| context.get::<AgentId>().ok().flatten())
                .map_or_else(|| ROOT_AGENT.to_string(), |id| id.0);
            self.state.set_current_call(&agent, call.to_string());
        }
        DispatchAction::Proceed
    }
}

pub struct AgentRuntime {
    pub agent: Agent,
    pub tool_names: Vec<&'static str>,
    pub state: Arc<SessionState>,
    pub session: Session,
    pub models: Models,
    pub config: Config,
    pub roots: Vec<PathBuf>,
    pub mode: PermissionMode,
    pipeline: Arc<PermissionPipeline>,
    seen: Seen,
    setup: Setup,
}

/// The parts of the setup a rebuild needs again.
struct Setup {
    config_dir: PathBuf,
    data_dir: PathBuf,
    scrub_env: Vec<OsString>,
    git_bash: Option<GitBash>,
    system_prompt: Option<String>,
    append_system_prompt: Option<String>,
    json_schema: Option<Schema>,
}

impl AgentRuntime {
    pub fn new(setup: AgentSetup) -> Result<AgentRuntime, String> {
        let scope = Arc::new(Scope::new(
            setup.roots[0].clone(),
            setup.roots[1..].iter().cloned(),
        ));
        let permissions = &setup.config.permissions;
        let state = Arc::new(SessionState::new(StateParams {
            session_id: setup.session.id.clone(),
            mode: setup.mode,
            scope,
            deny: permissions.deny.clone(),
            ask: permissions.ask.clone(),
            allow: permissions.allow.clone(),
            ledger: setup.session.ledger.clone(),
            audit: setup.session.audit.clone(),
            home: setup.home,
            protected_dirs: [&setup.config_dir, &setup.data_dir]
                .into_iter()
                .map(|dir| std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone()))
                .collect(),
        }));
        let pipeline = Arc::new(PermissionPipeline::new(PipelineParams {
            state: state.clone(),
            approver: setup.approver,
            headless: setup.headless,
            hooks: Vec::new(),
            allow_extra: setup.config.shell_preclassifier_allow_extra.clone(),
            user_config: setup.user_config,
        }));
        let setup_rest = Setup {
            config_dir: setup.config_dir,
            data_dir: setup.data_dir,
            scrub_env: setup.scrub_env,
            git_bash: setup.git_bash,
            system_prompt: setup.system_prompt,
            append_system_prompt: setup.append_system_prompt,
            json_schema: setup.json_schema,
        };
        let seen = Seen::default();
        let (agent, tool_names) = build_agent(&Parts {
            setup: &setup_rest,
            roots: &setup.roots,
            pipeline: &pipeline,
            state: &state,
            seen: &seen,
            models: &setup.models,
            config: &setup.config,
            session: &setup.session,
        })?;
        Ok(AgentRuntime {
            agent,
            tool_names,
            state,
            session: setup.session,
            models: setup.models,
            config: setup.config,
            roots: setup.roots,
            mode: setup.mode,
            pipeline,
            seen,
            setup: setup_rest,
        })
    }

    /// Rebuilds the registry and the agent for the current scope. The conversation lives in the
    /// session store, so a new agent continues it.
    fn rebuild(&mut self) -> Result<(), String> {
        let (agent, tool_names) = build_agent(&Parts {
            setup: &self.setup,
            roots: &self.roots,
            pipeline: &self.pipeline,
            state: &self.state,
            seen: &self.seen,
            models: &self.models,
            config: &self.config,
            session: &self.session,
        })?;
        self.agent = agent;
        self.tool_names = tool_names;
        Ok(())
    }

    pub fn state(&self) -> &Arc<SessionState> {
        &self.state
    }

    /// Adds a workspace root from `/add-dir`: validated and canonicalized like `--add-dir`, then
    /// the scope, registry and agent are rebuilt for the next turn. The conversation and the
    /// session store are kept. Returns the canonical root.
    pub fn add_dir(&mut self, entry: &Path, launch_dir: &Path) -> Result<PathBuf, String> {
        let resolved = workspace::resolve(
            &self.roots[0],
            launch_dir,
            [(RootSource::Command, entry.to_path_buf())],
        );
        if let Some(warning) = resolved.warnings.first() {
            return Err(warning.clone());
        }
        let root = resolved
            .roots
            .into_iter()
            .nth(1)
            .ok_or_else(|| "that is the primary workspace root".to_string())?;
        if self.roots.contains(&root) {
            return Err(format!("{} is already a workspace root", root.display()));
        }
        let previous_scope = self.state.scope();
        self.state
            .set_scope(Arc::new(previous_scope.with_root(root.clone())));
        self.roots.push(root.clone());
        if let Err(e) = self.rebuild() {
            self.roots.pop();
            self.state.set_scope(previous_scope);
            return Err(e);
        }
        self.state.append(Record::AddDir {
            root: root.display().to_string(),
        });
        Ok(root)
    }

    /// The facts the ledger opens with: the roots, trust, git remotes and the pre-session file
    /// index.
    pub async fn record_session_start(&self) {
        let roots = self.roots.clone();
        let (index, git_remotes) = tokio::task::spawn_blocking(move || {
            let index = FileIndex::build(&roots, FILE_INDEX_CAP);
            let mut remotes: Vec<String> =
                roots.iter().flat_map(|root| git_remotes(root)).collect();
            remotes.sort();
            remotes.dedup();
            (index, remotes)
        })
        .await
        .unwrap_or_default();
        let trusted = trust::is_trusted(&self.setup.data_dir, &self.config.workspace);
        self.append(Record::SessionStart {
            session_id: self.session.id.clone(),
            permission_mode: self.mode.as_str().to_string(),
            workspace_roots: self
                .roots
                .iter()
                .map(|root| root.display().to_string())
                .collect(),
            workspace_trust: if trusted { "trusted" } else { "untrusted" }.to_string(),
            git_remotes,
        });
        self.append(Record::FileIndex {
            paths: index.paths,
            truncated: index.truncated,
        });
    }

    pub fn append(&self, record: Record) {
        if let Err(e) = self.session.ledger.append(record) {
            tracing::warn!(error = %e, "the session ledger could not be written");
            eprintln!("fontaine: warning: the session ledger could not be written: {e}");
        }
    }
}

struct Parts<'a> {
    setup: &'a Setup,
    roots: &'a [PathBuf],
    pipeline: &'a Arc<PermissionPipeline>,
    state: &'a Arc<SessionState>,
    seen: &'a Seen,
    models: &'a Models,
    config: &'a Config,
    session: &'a Session,
}

/// The agent, with its wrapped tool registry, and the names of the tools it was given.
fn build_agent(p: &Parts<'_>) -> Result<(Agent, Vec<&'static str>), String> {
    let instructions = prompt::load_instructions(&p.setup.config_dir, &p.roots[0]);
    let preamble = prompt::assemble(&PromptParts {
        roots: p.roots,
        system_prompt: p.setup.system_prompt.as_deref(),
        instructions: &instructions,
        append: p.setup.append_system_prompt.as_deref(),
    });
    let registered = tools::registry(RegistryParams {
        gate: p.pipeline.clone(),
        scope: p.state.scope(),
        seen: p.seen.clone(),
        scrub_env: p.setup.scrub_env.clone(),
        git_bash: p.setup.git_bash.clone(),
    })?;
    let names: Vec<&'static str> = registered.iter().map(|tool| tool.name).collect();
    let telemetry = &p.config.telemetry;
    let content = telemetry.enabled
        && (telemetry.log_user_prompts
            || telemetry.log_assistant_responses
            || telemetry.log_tool_details
            || telemetry.log_tool_content);
    let mut builder = AgentBuilder::new(p.models.main.completion.clone())
        .preamble(preamble)
        .record_content_telemetry(content)
        .memory(p.session.store())
        .conversation(CONVERSATION_LOG)
        .add_hook(CurrentCall {
            state: p.state.clone(),
        })
        .dynamic_tools(registered.into_iter().map(|tool| tool.tool).collect());
    if let Some(schema) = &p.setup.json_schema {
        builder = builder.output_schema_raw(schema.clone());
    }
    Ok((builder.build(), names))
}
