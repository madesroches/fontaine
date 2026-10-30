//! Building the root agent: its prompt, its wrapped tools and the builder configuration it
//! shares with subagents.

use std::path::PathBuf;
use std::sync::Arc;

use fontaine_agents::registry::AgentTypes;
use fontaine_agents::spawn::{AgentTool, SendMessageTool};
use fontaine_tools::ask_user::AskUserTool;
use fontaine_tools::builtins::Seen;
use fontaine_tools::gate::{AgentId, ROOT_AGENT};
use fontaine_tools::lsp::LspTool;
use fontaine_tools::result_pipeline::ResultPipeline;
use fontaine_tools::schedule::{
    CronCreateTool, CronDeleteTool, CronJobsTool, ScheduleStore, ScheduleWakeupTool,
};
use fontaine_tools::skill::SkillTool;
use fontaine_tools::todo::TodoTool;
use rig_agent::agent::{AgentHook, DispatchAction, DispatchEvent, HookContext};
use rig_agent::prelude::*;
use rig_agent::tool::server::ToolServerHandle;

use super::Setup;
use crate::checkpoint::StoreCapture;
use crate::child_runner::{CoreChildRunner, CoreWake, LedgerWindow};
use crate::config::Config;
use crate::fork_context::ParentContextHook;
use crate::gate::SessionState;
use crate::instructions::{self, InstructionStage, Loaded, Sources};
use crate::memory;
use crate::models::Models;
use crate::native_search;
use crate::permissions::{Approver, PermissionPipeline};
use crate::prompt::{self, PromptParts};
use crate::questions::HumanQuestions;
use crate::runtime_error::RuntimeError;
use crate::schedule::ReportedSchedule;
use crate::session::{CONVERSATION_LOG, Session};
use crate::skills::{self, SkillRuntime};
use crate::steering::{Steering, SteeringHook};
use crate::todos::ReportedTodos;
use crate::tools::{self, ExtensionTool, ExtensionTools, RegistryParams};
use crate::turn_recovery::RecoveryHook;

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

/// What the root agent and every subagent are built with: the telemetry setting and the hook
/// that tells the permission state which model tool call a tool is serving. Applied before any
/// other hook so `CurrentCall` runs first.
pub(crate) fn apply_common(
    builder: AgentBuilder,
    state: &Arc<SessionState>,
    record_content: bool,
) -> AgentBuilder {
    builder
        .record_content_telemetry(record_content)
        .add_hook(CurrentCall {
            state: state.clone(),
        })
}

pub(super) struct Parts<'a> {
    pub(super) setup: &'a Setup,
    pub(super) roots: &'a [PathBuf],
    pub(super) pipeline: &'a Arc<PermissionPipeline>,
    pub(super) state: &'a Arc<SessionState>,
    pub(super) seen: &'a Seen,
    pub(super) models: &'a Models,
    pub(super) config: &'a Config,
    pub(super) session: &'a Session,
    pub(super) tool_server: &'a ToolServerHandle,
    pub(super) results: &'a Arc<ResultPipeline>,
    pub(super) skills: &'a Arc<SkillRuntime>,
    pub(super) approver: &'a Arc<dyn Approver>,
    pub(super) types: &'a Arc<AgentTypes>,
    pub(super) runner: &'a Arc<CoreChildRunner>,
    pub(super) output_style: Option<(String, String)>,
    pub(super) steering: &'a Steering,
    pub(super) instructions: &'a Arc<InstructionStage>,
}

/// The agent, with its wrapped tools registered in the session's tool server.
pub(super) fn build_agent(
    p: &Parts<'_>,
) -> Result<(Agent, prompt::PromptPieces, Loaded, bool), RuntimeError> {
    let (pieces, sources, loaded) = prompt_layers(p);
    p.instructions.warn_once(&loaded.warnings);
    if let Some(warning) = p.session.todos().take_warning() {
        p.approver.warn(&warning);
    }
    if let Some(warning) = p.session.goal().take_warning() {
        p.approver.warn(&warning);
    }
    if scheduling(p) {
        p.session.scheduler().settle_expired();
        if let Some(warning) = p.session.scheduler().take_warning() {
            p.approver.warn(&warning);
        }
    }
    register_tools(p)?;
    p.instructions.reseed(sources, &loaded);
    let (agent, native_search) = finish_builder(p, pieces.join());
    Ok((agent, pieces, loaded, native_search))
}

/// Whether the scheduling tools exist in this session.
fn scheduling(p: &Parts<'_>) -> bool {
    p.setup.scheduling && p.config.schedule.enabled
}

fn prompt_layers(p: &Parts<'_>) -> (prompt::PromptPieces, Sources, Loaded) {
    let sources = Sources::for_state(p.state, &p.setup.config_dir, p.setup.home.as_deref());
    let loaded = instructions::load(&sources);
    let skill_index = skills::index(p.skills.extensions());
    let memory = p.state.memory_root.as_deref().map(|dir| {
        let notes = memory::load(dir);
        p.instructions.warn_once(&notes.warnings);
        memory::layer(dir, notes.index.as_deref())
    });
    let pieces = prompt::layers(&PromptParts {
        roots: p.roots,
        scratch: p.state.scratch.as_deref(),
        memory_root: p.state.memory_root.as_deref(),
        system_prompt: p.setup.system_prompt.as_deref(),
        instructions: &loaded.files,
        memory: memory.as_deref(),
        skill_index: skill_index.as_deref(),
        scheduling: scheduling(p).then_some(prompt::SCHEDULING_PROMPT),
        output_style: p
            .output_style
            .as_ref()
            .map(|(name, text)| (name.as_str(), text.as_str())),
        append: p.setup.append_system_prompt.as_deref(),
    });
    (pieces, sources, loaded)
}

fn register_tools(p: &Parts<'_>) -> Result<(), RuntimeError> {
    let ledger_window: Arc<dyn fontaine_agents::root_ledger::RootLedger> =
        Arc::new(LedgerWindow(p.state.clone()));
    let websearch = p
        .config
        .websearch_config_with_notice(&*p.setup.env, &p.setup.data_dir, {
            let approver = p.approver.clone();
            Arc::new(move |message: &str| approver.warn(message))
        })
        .map_err(|e| RuntimeError::caused("cannot set up the websearch tool", e))?;
    p.session.checkpoints().configure(
        p.config.checkpoints.clone(),
        p.state.memory_root.clone(),
        p.approver.clone(),
    );
    let registered = tools::registry(RegistryParams {
        gate: p.pipeline.clone(),
        scope: p.state.scope(),
        seen: p.seen.clone(),
        scrub_env: p.setup.scrub_env.clone(),
        set_env: tools::subprocess_env(p.config),
        shell_max_timeout_secs: p.config.shell.max_timeout_secs,
        git_bash: p.setup.git_bash.clone(),
        powershell: p.setup.powershell.clone(),
        results: p.results.clone(),
        hide_mcp_names: !(p.config.telemetry.enabled && p.config.telemetry.log_tool_details),
        network: p.setup.network.clone(),
        websearch,
        searxng: p.setup.searxng.clone(),
        jobs: p.setup.jobs.clone(),
        agent_waits: Some(p.state.agents().clone()),
        wait_wake: Some(Arc::new(CoreWake {
            tree: p.state.agents().clone(),
            steering: p.steering.clone(),
        })),
        extension: Some(ExtensionTools(extension_tools(p, ledger_window))),
        pre_images: Some(Arc::new(StoreCapture(p.session.checkpoints().clone()))),
    })
    .map_err(RuntimeError::new)?;
    for tool in registered {
        if tool.name == "shell" {
            p.skills.set_shell(tool.tool.clone());
        }
        p.tool_server.add_dynamic_tool(tool.tool);
    }
    Ok(())
}

fn extension_tools(
    p: &Parts<'_>,
    ledger_window: Arc<dyn fontaine_agents::root_ledger::RootLedger>,
) -> Vec<ExtensionTool> {
    let mut tools = vec![
        ExtensionTool::new(AgentTool {
            tree: p.state.agents().clone(),
            types: p.types.clone(),
            runner: p.runner.clone(),
            ledger: ledger_window.clone(),
        }),
        ExtensionTool::new(SkillTool {
            source: p.skills.clone(),
        }),
        ExtensionTool::new(SendMessageTool {
            tree: p.state.agents().clone(),
            ledger: ledger_window,
        }),
        ExtensionTool::new(AskUserTool {
            source: Arc::new(HumanQuestions::new(p.approver.clone(), p.state.clone())),
        }),
        ExtensionTool::new(TodoTool {
            store: Arc::new(ReportedTodos {
                todos: p.session.todos().clone(),
                approver: p.approver.clone(),
            }),
        }),
    ];
    if p.setup.lsp.is_active() {
        tools.push(ExtensionTool::new(LspTool {
            servers: p.setup.lsp.clone(),
            workspace: super::lsp::workspace(p.state),
        }));
    }
    if scheduling(p) {
        let store: Arc<dyn ScheduleStore> = Arc::new(ReportedSchedule {
            scheduler: p.session.scheduler().clone(),
            approver: p.approver.clone(),
        });
        tools.extend([
            ExtensionTool::new(CronCreateTool {
                store: store.clone(),
            }),
            ExtensionTool::new(CronJobsTool {
                store: store.clone(),
            }),
            ExtensionTool::new(CronDeleteTool {
                store: store.clone(),
            }),
            ExtensionTool::new(ScheduleWakeupTool { store }),
        ]);
    }
    tools
}

/// The agent, and whether it carries the provider's search tool.
fn finish_builder(p: &Parts<'_>, preamble: String) -> (Agent, bool) {
    let builder = AgentBuilder::new(p.models.main.completion.clone())
        .preamble(preamble)
        .memory(p.session.store())
        .conversation(CONVERSATION_LOG);
    let mut builder = apply_common(builder, p.state, p.config.telemetry.records_content())
        .add_hook(SteeringHook::new(p.steering.clone(), p.state.clone()))
        .add_hook(ParentContextHook::new(
            p.runner.context.parent_context.clone(),
        ))
        .add_hook(RecoveryHook::new(
            p.session.store(),
            CONVERSATION_LOG,
            p.approver.clone(),
        ))
        .tool_server_handle(p.tool_server.clone());
    if let Some(schema) = &p.setup.json_schema {
        builder = builder.output_schema_raw(schema.clone());
    }
    let native = native_search::offered(p.config, p.models, p.state);
    if let Some(entry) = &native {
        builder = builder.additional_params(serde_json::json!({ "tools": [entry] }));
    }
    (builder.build(), native.is_some())
}
