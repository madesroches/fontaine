//! The agent of a session: the wrapped tool registry, the permission pipeline, the model and
//! its memory, assembled the same way for the headless surface and the TUI. `/add-dir` rebuilds
//! the registry and the agent through the same path between turns.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use fontaine_agents::mode::ModeSetting;
use fontaine_agents::registry::{AgentType, AgentTypes, ModelSpec, TypeOrigin};
use fontaine_agents::spawn::{AgentTool, SendMessageTool};
use fontaine_agents::tree::{AgentTree, Limits};
use fontaine_guardian::{
    AllowSettings, Guardian, Profile, Screen, ScreenSettings, Settings as GuardianSettings,
};
use fontaine_mcp::{Connected, ServerSpec, Transport, connect as connect_server};
use fontaine_tools::builtins::Seen;
use fontaine_tools::gate::{AgentId, CallView, ROOT_AGENT};
use fontaine_tools::git_bash::GitBash;
use fontaine_tools::guarded::Guard;
use fontaine_tools::result_pipeline::{ResultPipeline, StageKind};
use fontaine_tools::risk::RiskClass;
use fontaine_tools::scope::Scope;
use fontaine_tools::skill::SkillTool;
use rig_agent::agent::{AgentHook, DispatchAction, DispatchEvent, HookContext};
use rig_agent::prelude::*;
use rig_agent::tool::server::{ToolServer, ToolServerHandle};
use rig_core::completion::ToolDefinition;
use rig_core::schemars::Schema;
use rig_core::tool::ToolOutput;
use rig_memory::{estimate_tokens, load_async};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::accounts::AccountsError;
use crate::adjudication::project_instructions;
use crate::child_runner::{
    ChildContext, CoreChildRunner, LedgerWindow, MainModel, NotificationStage,
};
use crate::config::{Account, AgentMode, Config, McpServer, PermissionMode};
use crate::context::{self, ContextError, ContextUsage};
use crate::extensions::Extensions;
use crate::gate::{AutoContext, ScreenStage, SessionState, StateParams, no_guardian};
use crate::ledger::{FILE_INDEX_CAP, FileIndex, Record, git_remotes};
use crate::models::{
    AccountListing, GuardianChoice, GuardianModel, ModelRef, Models, SharedSwitcher, Switcher,
};
use crate::permissions::{Approver, PermissionPipeline, PipelineParams};
use crate::prompt::{self, PromptParts};
use crate::scanner_stage::{Backend, ScannerStage, ScannerStart, llm_backend};
use crate::session::{CONVERSATION_LOG, ClearError, Session};
use crate::skills::{self, SkillRuntime};
use crate::steering::{Steering, SteeringHook};
use crate::tools::{self, ExtensionTools, RegistryParams};
use crate::trust;
use crate::user_config;
use crate::user_hooks::{self, HookProcess, PostHookStage};
use crate::workspace::{self, RootSource};

pub type EnvLookup = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

/// How the session began, for the ledger and the surfaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionOrigin {
    Fresh,
    /// Reopened (`resume`, `continue`) or copied (`fork`) from the session `source`.
    Continued {
        source: String,
        how: String,
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
    /// The user settings file "always allow" appends to, and `/model` and `/guardian-model`
    /// write to.
    pub user_config: Option<PathBuf>,
    /// The guardian profile that is actually in force, after the containment check.
    pub guardian_profile: Profile,
    /// What to tell the user once at startup.
    pub notices: Vec<String>,
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
}

/// The guardian a session runs with, from the model that was resolved for it.
pub fn build_guardian(
    model: Option<&GuardianModel>,
    config: &Config,
    profile: Profile,
) -> Guardian {
    let settings = GuardianSettings {
        timeout: std::time::Duration::from_secs(config.guardian.timeout_secs),
        confidence_threshold: config.guardian.confidence_threshold,
        cache: config.guardian.cache,
        profile,
        priced: match model {
            Some(GuardianModel::Ready(handle)) => handle.catalog_billed(),
            _ => true,
        },
    };
    match model {
        Some(GuardianModel::Ready(handle)) => Guardian::new(
            handle.completion.clone(),
            handle.reference.to_string(),
            settings,
        )
        .with_additional_params(handle.thinking_off.clone()),
        Some(GuardianModel::Unavailable { reference, reason }) => {
            Guardian::unavailable(reference.to_string(), reason, settings)
        }
        None => {
            let guardian = no_guardian();
            Guardian::unavailable(
                guardian.label().to_string(),
                guardian
                    .unavailable_reason()
                    .unwrap_or_default()
                    .to_string(),
                settings,
            )
        }
    }
}

/// The default address of a local Ollama daemon.
const OLLAMA_URL: &str = "http://localhost:11434";

/// The screen stage `[guardian.screen]` configures, or `None` when it is not configured. The
/// model names an account of kind `ollama`, whose `base_url` is the daemon to ask.
pub fn build_screen(config: &Config, profile: Profile) -> Result<Option<ScreenStage>, String> {
    let Some(screen) = &config.guardian.screen else {
        return Ok(None);
    };
    let (account, model) = screen
        .model
        .split_once('/')
        .ok_or_else(|| format!("`{}` is not `<account>/<model>`", screen.model))?;
    let account_config = config.accounts.get(account);
    let context_tokens = account_config.map_or(
        crate::config::DEFAULT_OLLAMA_CONTEXT_TOKENS,
        Account::ollama_context_tokens,
    );
    let base = account_config
        .and_then(|a| a.base_url.as_deref())
        .filter(|url| !url.is_empty())
        .unwrap_or(OLLAMA_URL);
    // `join` replaces the last path segment unless the base ends in a slash.
    let endpoint = url::Url::parse(&format!("{}/", base.trim_end_matches('/')))
        .map_err(|e| format!("the base_url of account `{account}` is not a URL: {e}"))?;
    let settings = ScreenSettings {
        endpoint,
        model: model.to_string(),
        context_tokens,
        question: screen.question,
        t_stop: screen.stop_threshold,
        timeout: std::time::Duration::from_millis(screen.timeout_ms),
        profile,
        allow: (screen.allow_threshold.is_some() || screen.shadow).then_some(AllowSettings {
            t_allow: screen.allow_threshold,
            steering_veto: screen.steering_veto,
        }),
    };
    Ok(Some(ScreenStage {
        screen: Arc::new(Screen::new(crate::transport::reqwest_client()?, settings)),
        label: screen.model.clone(),
        shadow: screen.shadow,
    }))
}

/// Tells the permission state which model tool call a tool is serving.
pub(crate) struct CurrentCall {
    pub(crate) state: Arc<SessionState>,
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

/// What a model switch did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Switched {
    pub reference: String,
    /// The guardian moved with the main model because it was on the main-model fallback.
    pub guardian_followed: bool,
    /// Why the choice could not be saved, when it could not.
    pub persist_error: Option<String>,
}

/// What a credential change did to the live session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CredentialEffect {
    /// The main model was on the account and was rebuilt on its current credential.
    pub main_rebuilt: bool,
    /// The main model is on the account but could not be rebuilt; it keeps the client it was
    /// built with.
    pub main_error: Option<String>,
    /// The guardian is on the account: whether it can be asked now.
    pub guardian_ready: Option<bool>,
}

pub struct AgentRuntime {
    pub agent: Agent,
    prompt_pieces: prompt::PromptPieces,
    pub state: Arc<SessionState>,
    pub session: Session,
    pub models: Models,
    pub config: Config,
    pub roots: Vec<PathBuf>,
    pub mode: PermissionMode,
    pub guardian_profile: Profile,
    pub notices: Vec<String>,
    /// Scanner startup notices. Kept apart from `notices` because every surface shows them in
    /// every permission mode.
    pub scanner_notices: Vec<String>,
    pub origin: SessionOrigin,
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
}

/// The parts of the setup a rebuild needs again.
pub(crate) struct Setup {
    config_dir: PathBuf,
    data_dir: PathBuf,
    pub(crate) scrub_env: Vec<OsString>,
    pub(crate) git_bash: Option<GitBash>,
    system_prompt: Option<String>,
    append_system_prompt: Option<String>,
    json_schema: Option<Schema>,
    user_config: Option<PathBuf>,
    switcher: Option<SharedSwitcher>,
    project_executables: bool,
    confirm_project_servers: bool,
    env: EnvLookup,
}

/// An MCP server the session may start, and whether the user has to say so first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerPlan {
    pub name: String,
    /// The command line or URL, as the approval prompt shows it.
    pub description: String,
    /// The repository defines it.
    pub project: bool,
    pub needs_approval: bool,
    /// Identifies this definition in the approval record.
    pub digest: String,
}

/// What starting an MCP server came to.
#[derive(Debug)]
pub struct McpOutcome {
    pub server: String,
    pub error: Option<String>,
}

fn server_digest(name: &str, server: &McpServer) -> String {
    let canonical = json!({
        "name": name,
        "command": server.command,
        "args": server.args,
        "env_vars": server.env_vars,
        "url": server.url,
    });
    let bytes =
        serde_jcs::to_vec(&canonical).unwrap_or_else(|_| canonical.to_string().into_bytes());
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn server_description(server: &McpServer) -> String {
    match (&server.command, &server.url) {
        (Some(command), _) => {
            let mut line = command.clone();
            for arg in server.args.iter().flatten() {
                line.push(' ');
                line.push_str(arg);
            }
            if let Some(names) = server.env_vars.as_ref().filter(|n| !n.is_empty()) {
                line.push_str(&format!("\nenvironment: {}", names.join(", ")));
            }
            line
        }
        (None, Some(url)) => url.clone(),
        (None, None) => String::new(),
    }
}

/// Every agent type a session has: the built-in ones, user agent files, `[agents.types]`
/// entries, and a repository's agent files last. Later definitions of a name replace earlier
/// ones, and a repository's can only have narrowed.
pub fn build_agent_types(config: &Config, extensions: &Extensions) -> AgentTypes {
    let mut types = AgentTypes::builtin();
    for agent in extensions
        .agent_types
        .iter()
        .filter(|t| matches!(t.origin, TypeOrigin::UserFile(_)))
    {
        types.insert(agent.clone());
    }
    for (name, ty) in &config.agents.types {
        types.insert(AgentType {
            name: name.clone(),
            description: format!("{name} (configured)"),
            prompt: None,
            tools: ty.tools.clone(),
            disallowed: Vec::new(),
            model: ty.model.as_deref().and_then(|m| ModelSpec::parse(m).ok()),
            mode: match ty.mode {
                None | Some(AgentMode::Inherit) => ModeSetting::Inherit,
                Some(AgentMode::Manual) => ModeSetting::Manual,
                Some(AgentMode::DontAsk) => ModeSetting::DontAsk,
            },
            origin: TypeOrigin::Config,
        });
    }
    for agent in extensions
        .agent_types
        .iter()
        .filter(|t| matches!(t.origin, TypeOrigin::ProjectFile(_)))
    {
        types.insert(agent.clone());
    }
    types
}

fn agent_limits(config: &Config) -> Limits {
    use fontaine_agents::budget::Budget;
    Limits {
        max_parallel: config.agents.max_parallel,
        subagent: Budget {
            tokens: Some(config.agents.subagent_token_budget),
            cost_usd: Some(config.agents.subagent_cost_budget_usd),
        },
        session: Budget {
            tokens: config.agents.session_token_budget,
            cost_usd: config.agents.session_cost_budget_usd,
        },
        ..Limits::default()
    }
}

impl AgentRuntime {
    pub fn new(mut setup: AgentSetup) -> Result<AgentRuntime, String> {
        setup
            .session
            .set_autocompact_window(setup.config.context.autocompact_window);
        let approver = setup.approver.clone();
        crate::providers::token_source::set_warn_sink(move |message| approver.warn(message));
        let scope = Arc::new(Scope::new(
            setup.roots[0].clone(),
            setup.roots[1..].iter().cloned(),
        ));
        let permissions = &setup.config.permissions;
        let auto_mode = &setup.config.auto_mode;
        let instructions = prompt::load_instructions(&setup.config_dir, &setup.roots[0]);
        let tree = Arc::new(AgentTree::new(agent_limits(&setup.config)));
        let telemetry = &setup.config.telemetry;
        let reveal_mcp_names = telemetry.enabled && telemetry.log_tool_details;
        let state = Arc::new(SessionState::new(StateParams {
            session_id: setup.session.id.clone(),
            mode: setup.mode,
            scope,
            deny: permissions.deny.clone(),
            ask: permissions.ask.clone(),
            allow: permissions.allow.clone(),
            ledger: setup.session.ledger.clone(),
            audit: setup.session.audit.clone(),
            approver: setup.approver.clone(),
            home: setup.home,
            protected_dirs: [&setup.config_dir, &setup.data_dir]
                .into_iter()
                .map(|dir| std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone()))
                .collect(),
            guardian: Arc::new(build_guardian(
                setup.models.guardian.as_ref(),
                &setup.config,
                setup.guardian_profile,
            )),
            auto: AutoContext {
                environment: fontaine_guardian::EnvironmentConfig::expand(&auto_mode.environment),
                rules: fontaine_guardian::RuleConfig::expand(
                    &auto_mode.hard_deny,
                    &auto_mode.soft_deny,
                    &auto_mode.allow,
                ),
                project_instructions: project_instructions(&instructions),
            },
            agents: tree.clone(),
            reveal_mcp_names,
        }));

        match build_screen(&setup.config, setup.guardian_profile) {
            Ok(stage) => {
                // Best-effort: a screen that never loads already reaches the user as an
                // unavailable-screen warning on its first call.
                if let (Some(stage), Ok(runtime)) = (&stage, tokio::runtime::Handle::try_current())
                {
                    let screen = stage.screen.clone();
                    runtime.spawn(async move {
                        let _ = screen.warm_up().await;
                    });
                }
                state.set_screen(stage)
            }
            Err(e) => setup
                .approver
                .warn(&format!("the guardian screen stage is off: {e}")),
        }

        let hook_process = Arc::new(HookProcess {
            scrub_env: setup.scrub_env.clone(),
            set_env: tools::subprocess_env(&setup.config),
            git_bash: setup.git_bash.clone(),
            project_dir: setup.roots[0].clone(),
        });
        let hooks = user_hooks::build(&setup.config, setup.project_executables, hook_process);
        let pipeline = Arc::new(PermissionPipeline::new(PipelineParams {
            state: state.clone(),
            approver: setup.approver.clone(),
            headless: setup.headless,
            hooks: hooks.pre,
            denied_hooks: hooks.denied,
            allow_extra: setup.config.shell_preclassifier_allow_extra.clone(),
            user_config: setup.user_config.clone(),
        }));
        let mut results = ResultPipeline::new().with_stage(
            StageKind::Notifications,
            Arc::new(NotificationStage { tree: tree.clone() }),
        );
        let mut llm_scanner = None;
        if let Some(backend) = setup.scanner.backend {
            let stage = ScannerStage::new(backend);
            if setup.config.guardian.scanner == crate::config::Scanner::Llm {
                llm_scanner = Some(stage.handle());
            }
            results = results.with_stage(StageKind::Scanner, Arc::new(stage));
        }
        if !hooks.post.is_empty() {
            results = results.with_stage(
                StageKind::PostToolUse,
                Arc::new(PostHookStage::new(hooks.post, state.clone())),
            );
        }
        let results = Arc::new(results);

        let extensions = Arc::new(setup.extensions);
        let agent_types = Arc::new(build_agent_types(&setup.config, &extensions));
        let skills = Arc::new(SkillRuntime::new(
            extensions.clone(),
            state.clone(),
            setup.project_executables,
        ));
        let main = Arc::new(RwLock::new(MainModel {
            handle: setup.models.main.clone(),
            listing: setup.models.main_listing.clone(),
        }));
        let tool_server = ToolServer::new().run();
        let setup_rest = Setup {
            config_dir: setup.config_dir,
            data_dir: setup.data_dir,
            scrub_env: setup.scrub_env,
            git_bash: setup.git_bash,
            system_prompt: setup.system_prompt,
            append_system_prompt: setup.append_system_prompt,
            json_schema: setup.json_schema,
            user_config: setup.user_config,
            switcher: setup
                .switcher
                .map(|switcher| Arc::new(RwLock::new(switcher))),
            project_executables: setup.project_executables,
            confirm_project_servers: setup.confirm_project_servers,
            env: setup.env,
        };
        let telemetry = &setup.config.telemetry;
        let record_content = telemetry.enabled
            && (telemetry.log_user_prompts
                || telemetry.log_assistant_responses
                || telemetry.log_tool_details
                || telemetry.log_tool_content);
        let preamble_state = state.clone();
        let preamble_config_dir = setup_rest.config_dir.clone();
        let preamble_system = setup_rest.system_prompt.clone();
        let runner = Arc::new(CoreChildRunner {
            context: Arc::new(ChildContext {
                state: state.clone(),
                tool_server: tool_server.clone(),
                main: main.clone(),
                switcher: setup_rest.switcher.clone(),
                preamble: Arc::new(move |agent_type: &AgentType| {
                    let roots: Vec<PathBuf> = preamble_state.scope().roots().to_vec();
                    let instructions = prompt::load_instructions(&preamble_config_dir, &roots[0]);
                    let layer =
                        prompt::subagent_layer(&agent_type.name, agent_type.prompt.as_deref());
                    prompt::assemble(&PromptParts {
                        roots: &roots,
                        system_prompt: preamble_system.as_deref(),
                        instructions: &instructions,
                        skill_index: None,
                        output_style: None,
                        append: Some(&layer),
                    })
                }),
                approver: setup.approver.clone(),
                record_content,
            }),
        });
        tree.set_root_model(setup.models.main.reference.to_string());
        {
            let handle = tool_server.clone();
            tree.set_root_tools(move || {
                handle
                    .toolset()
                    .names()
                    .map(str::to_owned)
                    .collect::<Vec<String>>()
            });
        }
        let seen = Seen::default();
        let steering = Steering::default();
        let (agent, prompt_pieces) = build_agent(&Parts {
            setup: &setup_rest,
            roots: &setup.roots,
            pipeline: &pipeline,
            state: &state,
            seen: &seen,
            models: &setup.models,
            config: &setup.config,
            session: &setup.session,
            tool_server: &tool_server,
            results: &results,
            skills: &skills,
            types: &agent_types,
            runner: &runner,
            output_style: None,
            steering: &steering,
        })?;
        Ok(AgentRuntime {
            agent,
            prompt_pieces,
            state,
            session: setup.session,
            models: setup.models,
            config: setup.config,
            roots: setup.roots,
            mode: setup.mode,
            guardian_profile: setup.guardian_profile,
            notices: setup.notices,
            scanner_notices: setup.scanner.notices,
            origin: setup.origin,
            steering,
            files: Arc::default(),
            pipeline,
            approver: setup.approver,
            seen,
            setup: setup_rest,
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
        })
    }

    /// Rebuilds the registry and the agent for the current scope. The conversation lives in the
    /// session store, so a new agent continues it.
    fn rebuild(&mut self) -> Result<(), String> {
        let (agent, prompt_pieces) = build_agent(&Parts {
            setup: &self.setup,
            roots: &self.roots,
            pipeline: &self.pipeline,
            state: &self.state,
            seen: &self.seen,
            models: &self.models,
            config: &self.config,
            session: &self.session,
            tool_server: &self.tool_server,
            results: &self.results,
            skills: &self.skills,
            types: &self.agent_types,
            runner: &self.runner,
            output_style: self.output_style_text(),
            steering: &self.steering,
        })?;
        self.agent = agent;
        self.prompt_pieces = prompt_pieces;
        Ok(())
    }

    fn output_style_text(&self) -> Option<(String, String)> {
        let name = self.output_style.as_deref()?;
        let style = self.extensions.output_style(name)?;
        Some((style.name.clone(), style.body.clone()))
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
    pub fn set_output_style(&mut self, name: Option<&str>) -> Result<(), String> {
        if let Some(name) = name
            && self.extensions.output_style(name).is_none()
        {
            let known: Vec<&str> = self
                .extensions
                .output_styles
                .iter()
                .map(|s| s.name.as_str())
                .collect();
            return Err(format!(
                "no output style named `{name}`; available: {}",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ));
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
    pub async fn expand_skill(&self, name: &str, arguments: &str) -> Result<String, String> {
        // A user-typed skill is no model call, so its commands must not inherit the previous
        // turn's tool_use_id.
        self.state.clear_current_call(ROOT_AGENT);
        // The grants the skill lends last for the turn its text starts; an expansion that is
        // abandoned or fails never starts one.
        let guard = crate::turn::EndTurnOnDrop(&self.state);
        let text = self
            .skills
            .expand(name, arguments, ROOT_AGENT, false)
            .await?;
        std::mem::forget(guard);
        Ok(text)
    }

    /// The reports and messages queued for the main conversation, as a prompt for a turn of
    /// its own. They pass through the result pipeline like any tool result.
    pub async fn notification_prompt(&self) -> Option<String> {
        let tree = self.state.agents();
        let notes = tree.take_notifications(ROOT_AGENT);
        if notes.is_empty() {
            return None;
        }
        let text = notes
            .iter()
            .map(fontaine_agents::tree::Notification::render)
            .collect::<Vec<_>>()
            .join("\n\n");
        let args = json!({});
        // No model call produced this result, so hooks get no tool_use_id for it.
        self.state.clear_current_call(ROOT_AGENT);
        let view = CallView {
            tool: "agent",
            risk: RiskClass::Arbitrary,
            args: &args,
            agent: ROOT_AGENT,
            annotations: None,
        };
        let (screened, _) = self.results.run(view, Ok(ToolOutput::text(text))).await;
        let screened = match screened {
            Ok(output) => output.render(),
            Err(error) => error.model_output().render(),
        };
        Some(format!(
            "[Harness note, not the user's words: agents you started or that wrote to you have news.]\n\n{screened}"
        ))
    }

    /// Whether the workspace was trusted, so project hooks and MCP servers loaded.
    pub fn project_executables(&self) -> bool {
        self.setup.project_executables
    }

    /// The MCP servers this session would start, and which of them need the user's say-so.
    pub fn mcp_plan(&self) -> Vec<McpServerPlan> {
        let root = &self.roots[0];
        self.config
            .mcp_servers
            .iter()
            .filter_map(|(name, server)| {
                let origin = self
                    .config
                    .mcp_origins
                    .get(name)
                    .copied()
                    .unwrap_or_default();
                let project = origin.is_project();
                if project && !self.setup.project_executables {
                    return None;
                }
                let digest = server_digest(name, server);
                // A repository's server needs the user's approval; one in the user's own
                // settings, or in an untracked local file of a trusted root, does not.
                let repository_defined =
                    origin.slot == crate::config::Slot::Project || (project && !origin.user_trust);
                let needs_approval = repository_defined
                    && self.setup.confirm_project_servers
                    && !trust::server_approved(&self.setup.data_dir, root, name, &digest);
                Some(McpServerPlan {
                    name: name.clone(),
                    description: server_description(server),
                    project,
                    needs_approval,
                    digest,
                })
            })
            .collect()
    }

    /// Starts the MCP servers the plan allows: those that need no approval and those named in
    /// `approved`. A server that fails to start is reported and skipped; the session goes on.
    pub async fn connect_mcp(&mut self, approved: &[String]) -> Vec<McpOutcome> {
        let telemetry = &self.config.telemetry;
        let reveal = telemetry.enabled && telemetry.log_tool_details;
        let guard = Guard {
            gate: self.pipeline.clone(),
            results: self.results.clone(),
            hide_mcp_names: !reveal,
        };
        let mut outcomes = Vec::new();
        for plan in self.mcp_plan() {
            if plan.needs_approval && !approved.contains(&plan.name) {
                continue;
            }
            let Some(server) = self.config.mcp_servers.get(&plan.name) else {
                continue;
            };
            let spec = ServerSpec {
                name: plan.name.clone(),
                transport: match (&server.command, &server.url) {
                    (Some(command), _) => Transport::Stdio {
                        command: command.clone(),
                        args: server.args.clone().unwrap_or_default(),
                        env_vars: server.env_vars.clone().unwrap_or_default(),
                        env: tools::subprocess_env(&self.config),
                        cwd: self.roots[0].clone(),
                    },
                    (None, Some(url)) => Transport::Http { url: url.clone() },
                    (None, None) => continue,
                },
            };
            let env = self.setup.env.clone();
            match connect_server(
                &spec,
                self.tool_server.clone(),
                guard.clone(),
                reveal,
                &move |name| env(name),
            )
            .await
            {
                Ok(connected) => {
                    self.mcp.push(connected);
                    outcomes.push(McpOutcome {
                        server: plan.name,
                        error: None,
                    });
                }
                Err(e) => {
                    self.approver.warn(&e.to_string());
                    outcomes.push(McpOutcome {
                        server: plan.name,
                        error: Some(e.to_string()),
                    });
                }
            }
        }
        outcomes
    }

    /// Records the user's approval of a repository-defined MCP server in the trust store.
    pub fn approve_mcp_server(&self, plan: &McpServerPlan) -> Result<(), String> {
        trust::approve_server(
            &self.setup.data_dir,
            &self.roots[0],
            &plan.name,
            &plan.digest,
        )
        .map_err(|e| e.to_string())
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

    /// Makes `model` the guardian from the next adjudication on. Its verdict cache starts empty.
    pub fn install_guardian(&self, model: &GuardianModel) {
        self.state.set_guardian(Arc::new(build_guardian(
            Some(model),
            &self.config,
            self.guardian_profile,
        )));
        if let Some(slot) = &self.llm_scanner {
            let timeout = std::time::Duration::from_secs(self.config.guardian.timeout_secs);
            *slot.write().unwrap_or_else(|e| e.into_inner()) = llm_backend(Some(model), timeout);
        }
    }

    /// A copy of the shared switcher, so no lock is held across an await.
    fn switcher(&self) -> Result<Switcher, String> {
        self.setup
            .switcher
            .as_ref()
            .map(|switcher| switcher.read().unwrap_or_else(|e| e.into_inner()).clone())
            .ok_or_else(|| "models cannot be switched in this session".to_string())
    }

    fn guardian_for(&self, text: &str) -> Result<GuardianModel, String> {
        self.switcher()?
            .guardian(
                text,
                Some((&self.models.main.reference, &self.models.main_listing)),
            )
            .map_err(|e| e.to_string())
    }

    fn persist(&self, write: impl FnOnce(&Path) -> std::io::Result<()>) -> Option<String> {
        match &self.setup.user_config {
            Some(path) => write(path)
                .err()
                .map(|e| format!("could not save it to {}: {e}", path.display())),
            None => Some("there is no user settings file to save it to".to_string()),
        }
    }

    /// `/model`: switches the live session's model, rebuilds the agent on it and saves the
    /// choice as `[model] model`. The guardian follows only when it is on the main-model
    /// fallback; a pinned or validated guardian stays where it is.
    pub async fn switch_main_model(&mut self, text: &str) -> Result<Switched, String> {
        let guardian_followed = self.install_main(text).await?;
        let reference = self.models.main.reference.to_string();
        let persist_error = self.persist(|path| user_config::set_model(path, &reference));
        Ok(Switched {
            reference,
            guardian_followed,
            persist_error,
        })
    }

    /// Resolves `text`, rebuilds the agent on it and makes it the model of the session and its
    /// subagents, without saving anything. Returns whether the guardian followed.
    async fn install_main(&mut self, text: &str) -> Result<bool, String> {
        let (handle, listing) = self
            .switcher()?
            .main(text)
            .await
            .map_err(|e| e.to_string())?;
        let previous = std::mem::replace(&mut self.models.main, handle);
        let previous_listing = std::mem::replace(&mut self.models.main_listing, listing);
        if let Err(e) = self.rebuild() {
            self.models.main = previous;
            self.models.main_listing = previous_listing;
            return Err(e);
        }
        *self.main.write().unwrap_or_else(|e| e.into_inner()) = MainModel {
            handle: self.models.main.clone(),
            listing: self.models.main_listing.clone(),
        };
        self.state
            .agents()
            .set_root_model(self.models.main.reference.to_string());
        let guardian_followed = self.models.guardian_choice == GuardianChoice::MainFallback;
        if guardian_followed {
            let model = GuardianModel::Ready(self.models.main.clone());
            self.install_guardian(&model);
            self.models.guardian = Some(model);
        }
        Ok(guardian_followed)
    }

    /// The models of every configured account, for the `/model` picker. The first call fetches
    /// them; later calls fetch only the accounts that failed before or were not listed yet, so
    /// a credential or account added mid-session shows up while cached successes make no
    /// request. The main account's entry always follows the current listing. With `account`,
    /// that account is fetched whether cached or not and only its listing is returned.
    pub async fn model_listings(
        &mut self,
        account: Option<&str>,
    ) -> Result<Vec<AccountListing>, String> {
        let switcher = self.switcher()?;
        let wanted: Option<Vec<String>> = match account {
            Some(name) => Some(vec![name.to_string()]),
            None if self.listings.is_empty() => None,
            None => Some(
                switcher
                    .config
                    .accounts
                    .keys()
                    .filter(|name| {
                        self.listings
                            .iter()
                            .find(|l| l.account == **name)
                            .is_none_or(|l| l.models.is_err())
                    })
                    .cloned()
                    .collect(),
            ),
        };
        let fetched = switcher
            .listings(
                Some((&self.models.main.reference, &self.models.main_listing)),
                wanted.as_deref(),
            )
            .await;
        for listing in fetched {
            match self
                .listings
                .iter_mut()
                .find(|l| l.account == listing.account)
            {
                Some(slot) => *slot = listing,
                None => self.listings.push(listing),
            }
        }
        self.listings.sort_by(|a, b| a.account.cmp(&b.account));
        let main = &self.models.main.reference.account;
        if let Some(slot) = self.listings.iter_mut().find(|l| l.account == *main) {
            slot.models = Ok(self.models.main_listing.clone());
        }
        Ok(match account {
            Some(name) => self
                .listings
                .iter()
                .filter(|l| l.account == name)
                .cloned()
                .collect(),
            None => self.listings.clone(),
        })
    }

    /// Installs an account the provider dialog added, in the session's configuration and in the
    /// switcher the subagents share. It is listed by the next [`AgentRuntime::model_listings`].
    pub fn account_added(&mut self, name: &str, account: Account) {
        self.config
            .accounts
            .insert(name.to_string(), account.clone());
        if let Some(switcher) = &self.setup.switcher {
            switcher
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .config
                .accounts
                .insert(name.to_string(), account);
        }
    }

    /// Forgets an account the provider dialog removed.
    pub fn account_removed(&mut self, name: &str) {
        self.config.accounts.remove(name);
        if let Some(switcher) = &self.setup.switcher {
            switcher
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .config
                .accounts
                .remove(name);
        }
        self.listings.retain(|l| l.account != name);
    }

    /// Whether the account may be removed: not while the live main model, the live guardian or
    /// the screen stage uses it.
    pub fn removable(&self, name: &str) -> Result<(), AccountsError> {
        crate::accounts::check_defined_only_in_user_file(&self.config, name)?;
        let on = |reference: &str| ModelRef::parse(reference).is_some_and(|r| r.account == name);
        if self.models.main.reference.account == name {
            return Err(AccountsError::InUse {
                by: "the main model",
            });
        }
        if on(self.state.guardian().label()) {
            return Err(AccountsError::InUse { by: "the guardian" });
        }
        if self
            .config
            .guardian
            .screen
            .as_ref()
            .is_some_and(|screen| on(&screen.model))
        {
            return Err(AccountsError::InUse {
                by: "the guardian's screen stage",
            });
        }
        Ok(())
    }

    /// Brings the session in line with a credential stored, replaced or removed for `name`: its
    /// cached listing is dropped, the main model is rebuilt when it is on the account, and the
    /// guardian is tried again when it is. A missing or invalid credential leaves the guardian
    /// unavailable, which fails closed. Nothing is saved to the settings.
    pub async fn credentials_changed(&mut self, name: &str) -> CredentialEffect {
        self.listings.retain(|l| l.account != name);
        let mut effect = CredentialEffect::default();
        if self.models.main.reference.account == name {
            let reference = self.models.main.reference.to_string();
            match self.install_main(&reference).await {
                Ok(_) => effect.main_rebuilt = true,
                Err(e) => effect.main_error = Some(e),
            }
        }
        let guardian_on_account = ModelRef::parse(self.state.guardian().label())
            .is_some_and(|reference| reference.account == name);
        if guardian_on_account {
            effect.guardian_ready = Some(self.retry_guardian().unwrap_or(false));
        }
        effect
    }

    /// `/guardian-model`: pins the guardian to `text` for the live session and saves it as
    /// `[guardian] model`. An account with no credential gives an unavailable guardian rather
    /// than an error, which the next adjudication reports.
    pub fn set_guardian_model(&mut self, text: &str) -> Result<Switched, String> {
        let model = self.guardian_for(text)?;
        self.install_guardian(&model);
        let reference = model.reference().to_string();
        self.models.guardian = Some(model);
        self.models.guardian_choice = GuardianChoice::Pinned;
        let persist_error = self.persist(|path| user_config::set_guardian_model(path, &reference));
        Ok(Switched {
            reference,
            guardian_followed: false,
            persist_error,
        })
    }

    /// Tries the guardian model again, for when its credential has appeared since startup.
    /// Returns whether it can be asked now.
    pub fn retry_guardian(&self) -> Result<bool, String> {
        let current = self.state.guardian();
        let model = self.guardian_for(current.label())?;
        self.install_guardian(&model);
        Ok(matches!(model, GuardianModel::Ready(_)))
    }

    /// The "pick another" answer to an unavailable guardian: switches the guardian and, when it
    /// can be asked, saves it as `[guardian] model`. `None` when the pick is unavailable too,
    /// which is not saved so the next session does not open on it.
    ///
    /// It takes `&self` because it answers a prompt raised mid-turn, so the runtime's own record
    /// of the guardian is not updated; the live guardian is the gate state's.
    pub fn use_guardian(&self, text: &str) -> Result<Option<Switched>, String> {
        let model = self.guardian_for(text)?;
        self.install_guardian(&model);
        if !matches!(model, GuardianModel::Ready(_)) {
            return Ok(None);
        }
        let reference = model.reference().to_string();
        let persist_error = self.persist(|path| user_config::set_guardian_model(path, &reference));
        Ok(Some(Switched {
            reference,
            guardian_followed: false,
            persist_error,
        }))
    }

    /// Guardian models to offer in place of an unavailable one.
    pub fn guardian_candidates(&self) -> Vec<ModelRef> {
        self.switcher().map_or_else(
            |_| Vec::new(),
            |switcher| switcher.candidates(&self.models.main.reference, &self.models.main_listing),
        )
    }

    /// The facts the ledger opens with: the roots, trust, git remotes, the names of the `[env]`
    /// overrides and the pre-session file index.
    pub async fn record_session_start(&self) {
        // A continued session keeps the file index it opened with: what it created since is
        // known to the ledger, and a fresh index would call those files pre-existing.
        let continued = matches!(self.origin, SessionOrigin::Continued { .. });
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
        self.set_files(index.paths.clone());
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
            env_keys: tools::subprocess_env(&self.config)
                .into_iter()
                .map(|(name, _)| name.to_string_lossy().into_owned())
                .collect(),
        });
        if let SessionOrigin::Continued { source, how } = &self.origin {
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

    fn set_files(&self, paths: Vec<String>) {
        *self.files.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(paths);
    }

    /// Rebuilds the completion file list in the background, so files the agent created and
    /// directories added since start become completable. Never blocks the caller.
    pub fn refresh_files(&self) {
        let roots = self.roots.clone();
        let slot = self.files.clone();
        tokio::task::spawn_blocking(move || {
            let index = FileIndex::build(&roots, FILE_INDEX_CAP);
            *slot.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(index.paths);
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
        if agents.any_running() || agents.has_notifications(ROOT_AGENT) {
            return Err(ClearError::SubagentsRunning);
        }
        self.session.clear().await?;
        self.state.append(Record::ContextCleared {});
        Ok(())
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
            system_prompt: context::text_tokens(&pieces.base) + count(&pieces.append),
            output_style: count(&pieces.output_style),
            instructions: pieces
                .instructions
                .iter()
                .map(|(path, text)| (path.clone(), context::text_tokens(text)))
                .collect(),
            skills: count(&pieces.skills),
            tools,
            mcp_tools,
            messages: estimate_tokens(conversation.messages()),
        })
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
    tool_server: &'a ToolServerHandle,
    results: &'a Arc<ResultPipeline>,
    skills: &'a Arc<SkillRuntime>,
    types: &'a Arc<AgentTypes>,
    runner: &'a Arc<CoreChildRunner>,
    output_style: Option<(String, String)>,
    steering: &'a Steering,
}

/// The agent, with its wrapped tools registered in the session's tool server.
fn build_agent(p: &Parts<'_>) -> Result<(Agent, prompt::PromptPieces), String> {
    let instructions = prompt::load_instructions(&p.setup.config_dir, &p.roots[0]);
    let skill_index = skills::index(p.skills.extensions());
    let pieces = prompt::layers(&PromptParts {
        roots: p.roots,
        system_prompt: p.setup.system_prompt.as_deref(),
        instructions: &instructions,
        skill_index: skill_index.as_deref(),
        output_style: p
            .output_style
            .as_ref()
            .map(|(name, text)| (name.as_str(), text.as_str())),
        append: p.setup.append_system_prompt.as_deref(),
    });
    let preamble = pieces.join();
    let ledger_window: Arc<dyn fontaine_agents::root_ledger::RootLedger> =
        Arc::new(LedgerWindow(p.state.clone()));
    let registered = tools::registry(RegistryParams {
        gate: p.pipeline.clone(),
        scope: p.state.scope(),
        seen: p.seen.clone(),
        scrub_env: p.setup.scrub_env.clone(),
        set_env: tools::subprocess_env(p.config),
        git_bash: p.setup.git_bash.clone(),
        results: p.results.clone(),
        hide_mcp_names: !(p.config.telemetry.enabled && p.config.telemetry.log_tool_details),
        extension: Some(ExtensionTools {
            agent: AgentTool {
                tree: p.state.agents().clone(),
                types: p.types.clone(),
                runner: p.runner.clone(),
                ledger: ledger_window.clone(),
            },
            send_message: SendMessageTool {
                tree: p.state.agents().clone(),
                ledger: ledger_window,
            },
            skill: SkillTool {
                source: p.skills.clone(),
            },
        }),
    })?;
    for tool in registered {
        if tool.name == "shell" {
            p.skills.set_shell(tool.tool.clone());
        }
        p.tool_server.add_dynamic_tool(tool.tool);
    }
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
        .add_hook(SteeringHook::new(p.steering.clone(), p.state.clone()))
        .tool_server_handle(p.tool_server.clone());
    if let Some(schema) = &p.setup.json_schema {
        builder = builder.output_schema_raw(schema.clone());
    }
    Ok((builder.build(), pieces))
}
