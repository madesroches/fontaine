//! The steps that assemble a session's shared parts, in the order a new runtime runs them.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use fontaine_agents::mode::ModeSetting;
use fontaine_agents::registry::{AgentType, AgentTypes, ModelSpec, TypeOrigin};
use fontaine_agents::tree::{AgentTree, Limits, NotificationKind};
use fontaine_guardian::composition::Composition;
use fontaine_guardian::{
    Guardian, ModeNotice, Profile, Reader, ReaderSettings, Settings as GuardianSettings,
};
use fontaine_tools::error_chain::chain;
use fontaine_tools::jobs::{JobNote, JobSink};
use fontaine_tools::kills::KillSwitches;
use fontaine_tools::lsp::DiagnosticsStage;
use fontaine_tools::result_pipeline::{ResultPipeline, StageKind};
use fontaine_tools::scope::Scope;
use rig_agent::tool::server::{ToolServer, ToolServerHandle};

use super::{AgentSetup, Setup};
use crate::adjudication::project_instructions;
use crate::child_runner::{ChildContext, CoreChildRunner, MainModel, NotificationStage};
use crate::config::{Account, AgentMode, Config};
use crate::extensions::Extensions;
use crate::fork_context::ParentContext;
use crate::gate::{
    AutoContext, AutoEdits, CompositionStage, SessionState, StateParams, no_guardian,
};
use crate::instructions::{self, InstructionStage, Sources};
use crate::models::{GuardianModel, ModelHandle, Models};
use crate::permissions::{Approver, PermissionPipeline, PipelineParams};
use crate::prompt::{self, PromptParts};
use crate::providers::PromptCaching;
use crate::providers::cache_minimum::min_cacheable_tokens;
use crate::runtime_error::RuntimeError;
use crate::scanner_stage::{Backend, ScannerStage};
use crate::skills::SkillRuntime;
use crate::steering::Steering;
use crate::tools;
use crate::user_hooks::{self, CommandHook, HookProcess, PostHookStage};

/// The guardian a session runs with, from the model that was resolved for it.
pub fn build_guardian(
    model: Option<&GuardianModel>,
    config: &Config,
    profile: Profile,
) -> Guardian {
    build_guardian_telling(model, config, profile, None)
}

/// [`build_guardian`] whose first resolution of the model's output transport is reported
/// through `approver`.
pub fn build_guardian_telling(
    model: Option<&GuardianModel>,
    config: &Config,
    profile: Profile,
    approver: Option<&Arc<dyn Approver>>,
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
        Some(GuardianModel::Ready(handle)) => {
            let label = handle.reference.to_string();
            let mut guardian = Guardian::new(handle.completion.clone(), label.clone(), settings)
                .with_additional_params(handle.thinking_off.clone());
            if let Some(probe) = &handle.output_probe {
                guardian = guardian.with_output_probe(probe.clone());
            }
            match approver {
                Some(approver) => {
                    if let Some(notice) = cache_floor_notice(handle, config, profile) {
                        approver.notice(&notice);
                    }
                    guardian.with_notifier(mode_notifier(approver.clone(), label))
                }
                None => guardian,
            }
        }
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

/// Approximate token sizes of the guardian's static prefix, its system prompt, per profile.
fn guardian_prefix_tokens(profile: Profile) -> u32 {
    match profile {
        Profile::Minimal => 1500,
        Profile::Full => 6000,
    }
}

/// Says so when the guardian's model caches nothing below a length its prefix does not reach,
/// which would leave the cache markers a silent no-op.
fn cache_floor_notice(handle: &ModelHandle, config: &Config, profile: Profile) -> Option<String> {
    if !handle.kind.provider().accepts_prompt_caching() {
        return None;
    }
    let account = config.accounts.get(&handle.reference.account)?;
    if !PromptCaching::of(account).enabled {
        return None;
    }
    let minimum = min_cacheable_tokens(handle.kind.as_str(), &handle.reference.model)?;
    (minimum > guardian_prefix_tokens(profile)).then(|| {
        format!(
            "guardian model {} caches prompts of {minimum} tokens or more, longer than its \
             prefix, so its cache markers have no effect; a model with a lower minimum caches it",
            handle.reference
        )
    })
}

fn mode_notifier(approver: Arc<dyn Approver>, label: String) -> fontaine_guardian::Notifier {
    Arc::new(move |notice| {
        match notice {
            ModeNotice::Schema => approver.notice(&format!(
                "guardian model {label} has no tool support, so its verdicts use the schema-constrained output path"
            )),
            ModeNotice::Incapable(reason) => approver.warn(&format!(
                "guardian model {label} {reason}: every guardian-gated call is denied until the guardian model is changed"
            )),
        }
    })
}

/// Resolves the guardian's output transport in the background so a notice reaches the user at
/// startup. A call never waits on this: it resolves the transport itself when it needs it.
pub(crate) fn warm_output_mode(guardian: &Arc<Guardian>) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        let guardian = guardian.clone();
        runtime.spawn(async move {
            // The outcome is reported through the notifier and re-derived per call.
            let _ = guardian.resolve_mode().await;
        });
    }
}

/// The default address of a local Ollama daemon.
const OLLAMA_URL: &str = "http://localhost:11434";

/// The reader of one `<account>/<model>`: the model names an account of kind `ollama`, whose
/// `base_url` is the daemon to ask.
fn build_reader(
    config: &Config,
    reference: &str,
    profile: Profile,
    timeout: std::time::Duration,
) -> Result<Reader, RuntimeError> {
    let (account, model) = reference
        .split_once('/')
        .ok_or_else(|| RuntimeError::new(format!("`{reference}` is not `<account>/<model>`")))?;
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
    let endpoint = url::Url::parse(&format!("{}/", base.trim_end_matches('/'))).map_err(|e| {
        RuntimeError::caused(
            format!("the base_url of account `{account}` is not a URL"),
            e,
        )
    })?;
    let network = config.loaded_network(Some(account)).map_err(|e| {
        RuntimeError::caused(format!("the network settings of account `{account}`"), e)
    })?;
    Ok(Reader::new(
        crate::transport::reqwest_client(&network)?,
        ReaderSettings {
            endpoint,
            model: model.to_string(),
            context_tokens,
            profile,
            timeout,
        },
    ))
}

/// The composition `[guardian.composition]` configures, or `None` when it is not configured.
pub fn build_composition(
    config: &Config,
    profile: Profile,
) -> Result<Option<CompositionStage>, RuntimeError> {
    let Some(composition) = &config.guardian.composition else {
        return Ok(None);
    };
    let spec = composition
        .spec()
        .map_err(|e| RuntimeError::caused("the guardian composition is not valid", e))?;
    let timeout = std::time::Duration::from_millis(composition.timeout_ms);
    let models = spec
        .read_models()
        .map_err(|e| RuntimeError::caused("the guardian composition is not valid", e))?;
    let readers = models
        .iter()
        .map(|model| {
            Ok((
                model.clone(),
                build_reader(config, model, profile, timeout)?,
            ))
        })
        .collect::<Result<Vec<_>, RuntimeError>>()?;
    let label = models.into_iter().collect::<Vec<_>>().join(",");
    let shadow = spec.shadow;
    let compiled = Composition::compile(spec, readers)
        .map_err(|e| RuntimeError::caused("the guardian composition is not valid", e))?;
    Ok(Some(CompositionStage {
        composition: Arc::new(compiled),
        label,
        shadow,
    }))
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
            inherits_context: false,
        });
    }
    for agent in extensions.agent_types.iter().filter(|t| {
        matches!(
            t.origin,
            TypeOrigin::ProjectFile(_) | TypeOrigin::PluginFile(_)
        )
    }) {
        types.insert(agent.clone());
    }
    types
}

pub(super) fn agent_limits(config: &Config) -> Limits {
    use fontaine_agents::budget::Budget;
    Limits {
        max_parallel: config.agents.max_parallel,
        subagent: Budget {
            tokens: config.agents.subagent_token_budget,
            cost_usd: config.agents.subagent_cost_budget_usd,
        },
        session: Budget {
            tokens: config.agents.session_token_budget,
            cost_usd: config.agents.session_cost_budget_usd,
        },
        ..Limits::default()
    }
}

/// The session's agent tree. The ledger of a resumed session anchors the id counter, so
/// new ids never collide with the per-agent logs a previous process persisted.
pub(super) fn agent_tree(setup: &AgentSetup) -> Arc<AgentTree> {
    Arc::new(AgentTree::new(
        agent_limits(&setup.config),
        setup.session.ledger.view().next_agent,
    ))
}

/// The permission state of a new session: its scope, rules, guardian and the agent tree.
pub(super) fn session_state(
    setup: &AgentSetup,
    tree: &Arc<AgentTree>,
    agent_types: &Arc<AgentTypes>,
) -> Arc<SessionState> {
    let scope = Arc::new(Scope::new(
        setup.roots[0].clone(),
        setup.roots[1..].iter().cloned(),
    ));
    let permissions = &setup.config.permissions;
    let (environment, rules) = setup.config.effective_auto_mode();
    let sources = Sources::new(
        setup.config_dir.clone(),
        setup.home.clone(),
        scope.clone(),
        permissions.deny.iter().map(|r| r.rule.clone()).collect(),
    );
    let launch = instructions::load(&sources);
    let telemetry = &setup.config.telemetry;
    Arc::new(SessionState::new(StateParams {
        session_id: setup.session.id.clone(),
        mode: setup.mode,
        scope,
        deny: permissions.deny.clone(),
        ask: permissions.ask.clone(),
        allow: permissions.allow.clone(),
        ledger: setup.session.ledger.clone(),
        audit: setup.session.audit.clone(),
        approver: setup.approver.clone(),
        home: setup.home.clone(),
        protected_dirs: [&setup.config_dir, &setup.data_dir]
            .into_iter()
            .map(|dir| std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone()))
            .collect(),
        scratch: setup.scratch.clone(),
        memory_root: setup.memory_root.clone(),
        guardian: {
            let guardian = Arc::new(build_guardian_telling(
                setup.models.guardian.as_ref(),
                &setup.config,
                setup.guardian_profile,
                Some(&setup.approver),
            ));
            warm_output_mode(&guardian);
            guardian
        },
        auto: AutoContext {
            environment,
            rules,
            project_instructions: project_instructions(&instructions::guardian_order(
                &launch.files,
            )),
        },
        agents: tree.clone(),
        agent_types: agent_types.clone(),
        reveal_mcp_names: telemetry.enabled && telemetry.log_tool_details,
        auto_edits: AutoEdits {
            tracked: permissions.auto_tracked_edits,
            scratch: permissions.auto_scratch_writes,
        },
    }))
}

/// Puts the configured composition on the state. One that cannot be built is reported and left
/// off.
pub(super) fn attach_composition(state: &SessionState, setup: &AgentSetup) {
    match build_composition(&setup.config, setup.guardian_profile) {
        Ok(stage) => {
            // Best-effort: a reader that never loads already reaches the user as an
            // unavailable warning on its first call.
            if let (Some(stage), Ok(runtime)) = (&stage, tokio::runtime::Handle::try_current()) {
                let composition = stage.composition.clone();
                runtime.spawn(async move {
                    let _ = composition.warm_up().await;
                });
            }
            state.set_composition(stage)
        }
        Err(e) => setup
            .approver
            .warn(&format!("the guardian composition is off: {}", chain(&e))),
    }
}

/// What stands between the model and its tools, built together because the result pipeline
/// carries the instruction stage.
pub(super) struct Gates {
    pub(super) pipeline: Arc<PermissionPipeline>,
    pub(super) retained: Setup,
    pub(super) instruction_stage: Arc<InstructionStage>,
    pub(super) results: Arc<ResultPipeline>,
    pub(super) llm_scanner: Option<Arc<RwLock<Backend>>>,
}

pub(super) fn gates(
    setup: &mut AgentSetup,
    tree: &Arc<AgentTree>,
    state: &Arc<SessionState>,
) -> Result<Gates, RuntimeError> {
    super::warn_if_no_search(setup);
    let (pipeline, post_hooks) = permission_pipeline(setup, state);
    let retained = retained(setup, state)?;
    retained
        .jobs
        .set_sink(Arc::new(TreeJobs { tree: tree.clone() }));
    let instruction_stage = instruction_stage(setup, state);
    let (results, llm_scanner) = result_pipeline(
        setup.scanner.backend.take(),
        &setup.config,
        tree,
        state,
        post_hooks,
        &instruction_stage,
        retained.lsp.is_active().then(|| DiagnosticsStage {
            servers: retained.lsp.clone(),
            workspace: super::lsp::workspace(state),
        }),
    );
    Ok(Gates {
        pipeline,
        retained,
        instruction_stage,
        results,
        llm_scanner,
    })
}

/// Queues a job's news in the main conversation's inbox, where the surface that runs turns
/// finds it.
struct TreeJobs {
    tree: Arc<AgentTree>,
}

impl JobSink for TreeJobs {
    fn post(&self, note: JobNote) {
        self.tree
            .post_to_root(&note.job, NotificationKind::Job, note.text);
    }
}

/// The stage that brings nested instruction files into tool results, wired to the session's
/// `/clear` and compaction so the main agent receives them again afterwards.
pub(super) fn instruction_stage(
    setup: &AgentSetup,
    state: &Arc<SessionState>,
) -> Arc<InstructionStage> {
    let stage = Arc::new(InstructionStage::new(
        Sources::for_state(state, &setup.config_dir, setup.home.as_deref()),
        setup.approver.clone(),
    ));
    let on_reset = stage.clone();
    setup
        .session
        .on_context_reset(Arc::new(move || on_reset.reset_main()));
    stage
}

/// The pre-tool pipeline, and the post-tool hooks the result pipeline runs.
pub(super) fn permission_pipeline(
    setup: &AgentSetup,
    state: &Arc<SessionState>,
) -> (Arc<PermissionPipeline>, Vec<CommandHook>) {
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
    (pipeline, hooks.post)
}

/// The stages every tool result passes through, and the `llm` scanner's slot when that is the
/// scanner in use.
pub(super) fn result_pipeline(
    backend: Option<Backend>,
    config: &Config,
    tree: &Arc<AgentTree>,
    state: &Arc<SessionState>,
    post_hooks: Vec<CommandHook>,
    instructions: &Arc<InstructionStage>,
    diagnostics: Option<DiagnosticsStage>,
) -> (Arc<ResultPipeline>, Option<Arc<RwLock<Backend>>>) {
    let mut results = ResultPipeline::new()
        .with_stage(
            StageKind::Notifications,
            Arc::new(NotificationStage { tree: tree.clone() }),
        )
        .with_stage(StageKind::Instructions, instructions.clone());
    if let Some(stage) = diagnostics {
        results = results.with_stage(StageKind::Diagnostics, Arc::new(stage));
    }
    let mut llm_scanner = None;
    if let Some(backend) = backend {
        let stage = ScannerStage::new(backend);
        if config.guardian.scanner == crate::config::Scanner::Llm {
            llm_scanner = Some(stage.handle());
        }
        results = results.with_stage(StageKind::Scanner, Arc::new(stage));
    }
    if !post_hooks.is_empty() {
        results = results.with_stage(
            StageKind::PostToolUse,
            Arc::new(PostHookStage::new(post_hooks, state.clone())),
        );
    }
    (Arc::new(results), llm_scanner)
}

/// The parts of the setup a rebuild needs again.
pub(super) fn retained(
    setup: &AgentSetup,
    state: &Arc<SessionState>,
) -> Result<Setup, RuntimeError> {
    let network = setup
        .config
        .loaded_network(None)
        .map(Arc::new)
        .map_err(|e| RuntimeError::caused("invalid [network] settings", e))?;
    Ok(Setup {
        network,
        config_dir: setup.config_dir.clone(),
        home: setup.home.clone(),
        data_dir: setup.data_dir.clone(),
        scrub_env: setup.scrub_env.clone(),
        git_bash: setup.git_bash.clone(),
        powershell: setup.powershell.clone(),
        system_prompt: setup.system_prompt.clone(),
        append_system_prompt: setup.append_system_prompt.clone(),
        json_schema: setup.json_schema.clone(),
        user_config: setup.user_config.clone(),
        switcher: setup
            .switcher
            .clone()
            .map(|switcher| Arc::new(RwLock::new(switcher))),
        project_executables: setup.project_executables,
        confirm_project_servers: setup.confirm_project_servers,
        scheduling: setup.scheduling,
        env: setup.env.clone(),
        searxng: Arc::default(),
        jobs: Arc::default(),
        lsp: super::lsp::build(&setup.config, state, setup.env.clone()),
        store: setup.session.store(),
    })
}

/// What the subagent runner borrows from the session being built.
pub(super) struct ChildDeps<'a> {
    retained: &'a Setup,
    state: &'a Arc<SessionState>,
    approver: &'a Arc<dyn Approver>,
    steering: &'a Steering,
    results: &'a Arc<ResultPipeline>,
}

impl<'a> ChildDeps<'a> {
    pub(super) fn new(
        retained: &'a Setup,
        state: &'a Arc<SessionState>,
        approver: &'a Arc<dyn Approver>,
        steering: &'a Steering,
        results: &'a Arc<ResultPipeline>,
    ) -> Self {
        Self {
            retained,
            state,
            approver,
            steering,
            results,
        }
    }
}

/// The main model slot and the tool server the root agent and its subagents share, the runner
/// that starts the subagents, and the kill switches the runner and the tools both take. The
/// tree is wired to the model and tools.
pub(super) fn subagent_support(
    deps: &ChildDeps<'_>,
    tree: &AgentTree,
    models: &Models,
    config: &Config,
) -> (
    Arc<RwLock<MainModel>>,
    ToolServerHandle,
    Arc<CoreChildRunner>,
    Arc<KillSwitches>,
) {
    let main = Arc::new(RwLock::new(MainModel {
        handle: models.main.clone(),
        listing: models.main_listing.clone(),
    }));
    let tool_server = ToolServer::new().run();
    let kills = Arc::new(KillSwitches::new());
    let runner = child_runner(
        deps,
        &tool_server,
        &main,
        config.telemetry.records_content(),
        &kills,
    );
    wire_root_tree(tree, models, &tool_server);
    (main, tool_server, runner, kills)
}

/// The runner that starts subagents on the session's tools and models.
fn child_runner(
    deps: &ChildDeps<'_>,
    tool_server: &ToolServerHandle,
    main: &Arc<RwLock<MainModel>>,
    record_content: bool,
    kills: &Arc<KillSwitches>,
) -> Arc<CoreChildRunner> {
    let ChildDeps {
        retained,
        state,
        approver,
        steering,
        results,
    } = *deps;
    let preamble_state = state.clone();
    let preamble_config_dir = retained.config_dir.clone();
    let preamble_home = retained.home.clone();
    let preamble_system = retained.system_prompt.clone();
    Arc::new(CoreChildRunner {
        context: Arc::new(ChildContext {
            state: state.clone(),
            tool_server: tool_server.clone(),
            main: main.clone(),
            switcher: retained.switcher.clone(),
            preamble: Arc::new(move |agent_type: &AgentType| {
                let roots: Vec<PathBuf> = preamble_state.scope().roots().to_vec();
                let sources = Sources::for_state(
                    &preamble_state,
                    &preamble_config_dir,
                    preamble_home.as_deref(),
                );
                let instructions = instructions::load(&sources).files;
                let layer = if agent_type.inherits_context {
                    prompt::fork_layer()
                } else {
                    prompt::subagent_layer(&agent_type.name, agent_type.prompt.as_deref())
                };
                prompt::assemble(&PromptParts {
                    roots: &roots,
                    scratch: preamble_state.scratch.as_deref(),
                    memory_root: preamble_state.memory_root.as_deref(),
                    system_prompt: preamble_system.as_deref(),
                    instructions: &instructions,
                    memory: None,
                    skill_index: None,
                    scheduling: None,
                    output_style: None,
                    append: Some(&layer),
                })
            }),
            approver: approver.clone(),
            record_content,
            parent_context: ParentContext::default(),
            steering: steering.clone(),
            results: results.clone(),
            store: retained.store.clone(),
            kills: kills.clone(),
        }),
    })
}

/// The session-wide settings that apply before anything is built.
pub(super) fn prepare(setup: &mut AgentSetup) {
    setup
        .session
        .set_autocompact_window(setup.config.context.autocompact_window);
    let approver = setup.approver.clone();
    crate::providers::token_source::set_warn_sink(move |message| approver.warn(message));
}

/// The loaded extensions with what is derived from them: the agent types and the skills.
pub(super) fn extension_runtimes(
    extensions: Extensions,
    state: &Arc<SessionState>,
    project_executables: bool,
) -> (Arc<Extensions>, Arc<SkillRuntime>) {
    let extensions = Arc::new(extensions);
    let skills = Arc::new(SkillRuntime::new(
        extensions.clone(),
        state.clone(),
        project_executables,
    ));
    (extensions, skills)
}

/// Tells the agent tree which model and tools the root agent has.
fn wire_root_tree(tree: &AgentTree, models: &Models, tool_server: &ToolServerHandle) {
    tree.set_root_model(models.main.reference.to_string());
    let handle = tool_server.clone();
    tree.set_root_tools(move || {
        handle
            .toolset()
            .names()
            .map(str::to_owned)
            .collect::<Vec<String>>()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_subagent_budgets_leave_the_child_uncapped() {
        let mut config = Config::defaults(PathBuf::from("."));
        let limits = agent_limits(&config);
        assert_eq!(limits.subagent.tokens, None);
        assert_eq!(limits.subagent.cost_usd, None);

        config.agents.subagent_token_budget = Some(50_000);
        config.agents.subagent_cost_budget_usd = Some(0.5);
        let limits = agent_limits(&config);
        assert_eq!(limits.subagent.tokens, Some(50_000));
        assert_eq!(limits.subagent.cost_usd, Some(0.5));
    }
}
