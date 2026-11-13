//! Running subagents. The `agent` tool, the registry and the budgets live in `fontaine-agents`;
//! this is what starts a child there: its own model, its own context, a tool set cut from the
//! parent's, and every call it makes through the same permission pipeline as everyone's.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use fontaine_agents::budget::Spend;
use fontaine_agents::registry::{AgentType, ModelSpec, entry_has_specifier};
use fontaine_agents::root_ledger::{AgentEvent, RootLedger};
use fontaine_agents::runner::{
    ChildHandle, ChildReport, ChildRunner, ChildStatus, RevivalRequest, SpawnRequest,
};
use fontaine_agents::tree::{AgentTree, Notification, NotificationKind};
use fontaine_guardian::WireIds;
use fontaine_tools::error_chain::chain;
use fontaine_tools::gate::{CallView, ROOT_AGENT};
use fontaine_tools::jobs::{WAKE_POLL, WaitWake};
use fontaine_tools::kills::{KILL_GRACE, KillSwitches};
use fontaine_tools::lock::{lock, lock_recovering, read};
use fontaine_tools::result_pipeline::{
    ResultPipeline, ResultStage, ToolOutcome, append_note, text_view,
};
use fontaine_tools::risk::RiskClass;
use futures::FutureExt as _;
use futures::future::BoxFuture;
use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, DispatchAction, DispatchEvent,
    HookContext,
};
use rig_agent::prelude::*;
use rig_agent::tool::server::{ToolServer, ToolServerHandle};
use rig_core::tool::ToolOutput;
use rig_memory::{DirStore, Store as _};
use serde_json::json;
use tokio::sync::watch;
use tracing::Instrument;

use crate::agent::apply_common;
use crate::agent::deferred::{DeferredAdvertiser, child_section};
use crate::fork_context::{FORK_NOTE, ParentContext};
use crate::gate::{AgentViewEvent, ChildRestrictions, SessionState, spend_of};
use crate::ledger::Record;
use crate::model_roles::{
    FALLBACK_ROLE, ModelChain, RoleChains, Roles, Tried, fall_back_notice, fall_through_notice,
};
use crate::models::{Listing, ModelHandle, ModelRef, SharedSwitcher};
use crate::permission_rules::parse_rule;
use crate::permissions::Approver;
use crate::quota::UsageRole;
use crate::steering::{Steering, SteeringHook};
use crate::turn::{
    Drive, Flow, RETRY_NOTE, SessionEvent, TurnFailure, TurnOutcome, drive, wait_out_usage_limit,
};
use crate::turn_recovery::{InflightHook, RecoveryHook};
use crate::usage_ledger::Served;

const TARGET: &str = "fontaine::telemetry";

/// The reason a killed run records when it settles: the report's words too.
const KILL_REASON: &str = "it was killed";

/// Stops a killed run at the hook boundaries that bracket a model turn, so the run settles
/// through rig's cancel path and its committed messages reach the recovery hook instead of
/// dying with a dropped drive future.
struct KillHook {
    kill: watch::Receiver<bool>,
}

impl KillHook {
    /// What `on_completion_call` answers.
    fn completion_call(&self) -> CompletionCallAction {
        if *self.kill.borrow() {
            CompletionCallAction::Stop(KILL_REASON.to_string())
        } else {
            CompletionCallAction::Continue
        }
    }

    /// What `on_dispatch` answers for a tool call (`tool` is whether the effect is one).
    fn dispatch(&self, tool: bool) -> DispatchAction {
        if tool && *self.kill.borrow() {
            DispatchAction::stop(KILL_REASON)
        } else {
            DispatchAction::Proceed
        }
    }
}

impl AgentHook for KillHook {
    async fn on_completion_call(
        &self,
        _ctx: &HookContext,
        _event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        // Fires before every model request: a kill that arrived mid-stream takes effect
        // before the next request is sent.
        self.completion_call()
    }

    async fn on_dispatch(&self, _ctx: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        // Fires before each tool call is dispatched: a kill between tool calls in a batch
        // stops the batch before the next call runs.
        self.dispatch(event.tool_name().is_some())
    }
}

/// The main model as children see it: what `$main` means, and the listing that vouches for a
/// model on its account.
#[derive(Clone)]
pub struct MainModel {
    pub handle: ModelHandle,
    /// The preference list `handle` is the active entry of. Only the root moves its active
    /// entry; a child on `$main` falls through for itself.
    pub chain: Arc<ModelChain>,
    pub listing: Listing,
}

/// Delivers the notifications queued for an agent along with the next tool result it reads.
/// It runs before the scanner, so what another agent said is scanned like any result.
pub struct NotificationStage {
    pub tree: Arc<AgentTree>,
}

impl ResultStage for NotificationStage {
    fn process<'a>(
        &'a self,
        call: CallView<'a>,
        outcome: ToolOutcome,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let notes = self.tree.take_notifications(call.agent);
            if notes.is_empty() {
                return outcome;
            }
            let text = notes
                .iter()
                .map(fontaine_agents::tree::Notification::render)
                .collect::<Vec<_>>()
                .join("\n\n");
            append_note(outcome, &text)
        })
    }
}

/// The reports, messages and job notes queued for `agent`, as a prompt for a turn of its own.
/// They pass through the result pipeline like any tool result: agent notes as `agent` results,
/// job output as `job_output` results.
pub async fn notification_prompt(
    state: &SessionState,
    results: &ResultPipeline,
    agent: &str,
) -> Option<String> {
    let notes = state.agents().take_notifications(agent);
    if notes.is_empty() {
        return None;
    }
    // No model call produced these results, so hooks get no tool_use_id for them.
    state.clear_current_call(agent);
    let (jobs, agents): (Vec<_>, Vec<_>) = notes
        .iter()
        .partition(|note| note.kind == NotificationKind::Job);
    let mut screened = Vec::new();
    if !agents.is_empty() {
        screened.push(screened_notes(results, agent, "agent", RiskClass::Arbitrary, &agents).await);
    }
    if !jobs.is_empty() {
        screened
            .push(screened_notes(results, agent, "job_output", RiskClass::ReadOnly, &jobs).await);
    }
    let news = if !jobs.is_empty() {
        "agents or background jobs you started have news"
    } else if agents
        .iter()
        .all(|note| note.kind == NotificationKind::Reminder)
    {
        "tasks on your board are waiting for agents"
    } else {
        "agents you started or that wrote to you have news"
    };
    Some(format!(
        "[Harness note, not the user's words: {news}.]\n\n{}",
        screened.join("\n\n")
    ))
}

async fn screened_notes(
    results: &ResultPipeline,
    agent: &str,
    tool: &str,
    risk: RiskClass,
    notes: &[&Notification],
) -> String {
    let text = notes
        .iter()
        .map(|note| note.render())
        .collect::<Vec<_>>()
        .join("\n\n");
    let args = json!({});
    let view = CallView {
        tool,
        risk,
        args: &args,
        agent,
        annotations: None,
        requires_user_interaction: false,
        synthetic: true,
    };
    let (screened, _) = results.run(view, Ok(ToolOutput::text(text))).await;
    match screened {
        Ok(output) => text_view(&output),
        Err(error) => text_view(error.model_output()),
    }
}

/// What ends a parked `job_output` wait early: a message from another agent, a message the
/// user typed to the waiting agent meanwhile, and for the root a command, skill or `!` line the user queued.
pub struct CoreWake {
    pub tree: Arc<AgentTree>,
    pub steering: Steering,
}

impl WaitWake for CoreWake {
    fn pending(&self, agent: &str) -> bool {
        self.tree.has_message(agent)
            || self.steering.has_deliverable_message_for(agent)
            || (agent == ROOT_AGENT && self.steering.has_unannounced())
    }

    fn take_notice(&self, agent: &str) -> Option<String> {
        (agent == ROOT_AGENT)
            .then(|| self.steering.take_announcement())
            .flatten()
    }
}

/// The root ledger, as the agent tree sees it.
pub struct LedgerWindow(pub Arc<SessionState>);

impl RootLedger for LedgerWindow {
    fn human_messages(&self) -> Vec<String> {
        self.0.ledger.view().user_messages.clone()
    }

    fn append_event(&self, event: AgentEvent) {
        let record = match event {
            AgentEvent::Spawned {
                agent,
                name,
                parent,
                agent_type,
                model,
            } => Record::Agent {
                event: "spawned".into(),
                agent,
                parent: Some(parent),
                detail: Some(agent_type),
                name: Some(name),
                model,
            },
            AgentEvent::Finished { agent, status } => Record::Agent {
                event: "finished".into(),
                agent,
                parent: None,
                detail: Some(status),
                name: None,
                model: None,
            },
            AgentEvent::Message { from, to } => Record::Agent {
                event: "message".into(),
                agent: from,
                parent: None,
                detail: Some(to),
                name: None,
                model: None,
            },
            AgentEvent::Resumed { agent } => Record::Agent {
                event: "resumed".into(),
                agent,
                parent: None,
                detail: None,
                name: None,
                model: None,
            },
            AgentEvent::TaskCreated { task, agent } => task_record("task_created", agent, task),
            AgentEvent::TaskClaimed { task, agent } => task_record("task_claimed", agent, task),
            AgentEvent::TaskCompleted { task, agent } => task_record("task_completed", agent, task),
            AgentEvent::TaskReleased { task, agent } => task_record("task_released", agent, task),
        };
        self.0.append(record);
    }
}

/// A board event as the ledger keeps it: who did it and to which task, never the task's text.
fn task_record(event: &str, agent: String, task: String) -> Record {
    Record::Agent {
        event: event.into(),
        agent,
        parent: None,
        detail: Some(task),
        name: None,
        model: None,
    }
}

type Preamble = Arc<dyn Fn(&AgentType) -> String + Send + Sync>;

/// What starting a child needs from the session.
pub struct ChildContext {
    pub state: Arc<SessionState>,
    pub tool_server: ToolServerHandle,
    pub main: Arc<RwLock<MainModel>>,
    pub switcher: Option<SharedSwitcher>,
    /// The session's model roles, and the chains every child on a role shares.
    pub roles: Arc<Roles>,
    pub chains: Arc<RoleChains>,
    /// Whether the run has an explicit cost cap, which skips unpriced fall-through entries.
    pub explicit_budget_cap: bool,
    /// `[model] max_output_tokens`, which lowers the output cap of every child turn.
    pub max_output_tokens: Option<u32>,
    pub preamble: Preamble,
    pub approver: Arc<dyn Approver>,
    pub record_content: bool,
    /// What the main agent's model last read, for the agent types that inherit context.
    pub parent_context: ParentContext,
    /// What the human types in a subagent's view, which the child reads with its tool results.
    pub steering: Steering,
    /// Screens the notes a joining child reads, as the main conversation's are.
    pub results: Arc<ResultPipeline>,
    /// The session store: each child's conversation persists under its own log, named for
    /// its agent id, beside the root transcript.
    pub store: DirStore,
    /// The session's kill switches: each spawn registers its receiver so a killed child's
    /// in-flight calls end at the tool choke point.
    pub kills: Arc<KillSwitches>,
    /// The session's MCP tool definitions and what each agent has loaded of the deferred ones.
    pub deferral: fontaine_tools::mcp::Deferral,
    pub loaded: fontaine_tools::mcp::LoadedSets,
}

pub struct CoreChildRunner {
    pub context: Arc<ChildContext>,
}

/// A copy of the parent's tool set cut down to `names`. The tools are the parent's own wrapped
/// ones, so a child's calls reach the same gate; only the set is smaller.
fn fork_tools(root: &ToolServerHandle, names: &[String]) -> ToolServerHandle {
    let mut set = root.toolset();
    let unwanted: Vec<String> = set
        .names()
        .filter(|name| !names.iter().any(|wanted| wanted == name))
        .map(str::to_owned)
        .collect();
    for name in unwanted {
        set.remove_tool(&name);
    }
    let handle = ToolServer::new().run();
    handle.add_tools(set);
    handle
}

/// What the agent file of `agent_type` forbids a child outright and what it limits to
/// specifier-matched calls, installed on the node at spawn and at rebuild.
pub(crate) fn restrictions(agent_type: &AgentType) -> ChildRestrictions {
    let rules = |entries: &[String]| {
        entries
            .iter()
            .filter(|entry| entry_has_specifier(entry))
            .map(|entry| parse_rule(entry).expect("validated when the definition was loaded"))
            .collect::<Vec<_>>()
    };
    // A tool listed bare is allowed whole, so a specifier for it in the same list adds nothing.
    let only = |entries: &[String]| {
        let bare: Vec<String> = entries
            .iter()
            .filter(|entry| !entry_has_specifier(entry))
            .map(|entry| parse_rule(entry).expect("validated when the definition was loaded"))
            .map(|rule| rule.tool)
            .collect();
        rules(entries)
            .into_iter()
            .filter(|rule| !bare.contains(&rule.tool))
            .collect::<Vec<_>>()
    };
    ChildRestrictions {
        only: agent_type.tools.as_deref().map(only).unwrap_or_default(),
        deny: rules(&agent_type.disallowed),
    }
}

/// A child's model with the list it is served from, when it has one.
struct Resolved {
    handle: ModelHandle,
    list: Option<ChildList>,
}

/// The preference list a child runs on and the entry it is on. A role's or an inline list's
/// chain is shared with every other child on it, so a settled entry is the next call's start; the
/// session's main chain is not moved by a child.
struct ChildList {
    chain: Arc<ModelChain>,
    index: AtomicUsize,
    /// Whether a fall-through moves the chain's active entry.
    settles: bool,
}

impl ChildContext {
    fn switcher(&self) -> Option<crate::models::Switcher> {
        // Cloned out of the lock: resolving a model awaits.
        self.switcher
            .as_ref()
            .map(|s| read(s, "model switcher").clone())
    }

    /// The model a child runs on. Each child gets its own completion for its id, so a kind that
    /// derives per-caller state (a CLI session) never shares it between two subagents.
    async fn model_for(&self, spec: &ModelSpec, id: &str) -> Result<Resolved, String> {
        let main = read(&self.main, "main model").clone();
        let switcher = self.switcher();
        let mut list = None;
        let handle = match spec {
            ModelSpec::Main => {
                let at = main
                    .chain
                    .entries
                    .iter()
                    .position(|entry| *entry == main.handle.reference)
                    .unwrap_or_else(|| main.chain.active());
                list = Some(ChildList {
                    chain: main.chain.clone(),
                    index: AtomicUsize::new(at),
                    settles: false,
                });
                main.handle
            }
            ModelSpec::GuardianDefault => match switcher {
                Some(switcher) => switcher
                    .guardian_default(&main.handle.reference, &main.listing.models)
                    .await
                    .map(|handle| handle.conversation_scoped())
                    .unwrap_or(main.handle),
                None => main.handle,
            },
            ModelSpec::Model(text) => {
                let Some(switcher) = switcher else {
                    return Err("models cannot be resolved in this session".to_string());
                };
                let (handle, _) = switcher.main(text).await.map_err(|e| e.to_string())?;
                handle
            }
            ModelSpec::Named(_) | ModelSpec::List(_) => {
                let Some(switcher) = switcher else {
                    return Err("models cannot be resolved in this session".to_string());
                };
                let chain = self.chains.chain_for(&self.roles, spec)?;
                let at = chain.active();
                let (handle, _) = switcher
                    .main(&chain.entries[at].to_string())
                    .await
                    .map_err(|e| e.to_string())?;
                list = Some(ChildList {
                    chain,
                    index: AtomicUsize::new(at),
                    settles: true,
                });
                handle
            }
        };
        Ok(Resolved {
            handle: handle.for_agent(id),
            list,
        })
    }

    /// The handle for one entry of a list, for a child that is moving to it.
    async fn entry_for(&self, reference: &ModelRef, id: &str) -> Result<ModelHandle, String> {
        let Some(switcher) = self.switcher() else {
            return Err("models cannot be resolved in this session".to_string());
        };
        let (handle, _) = switcher
            .main(&reference.to_string())
            .await
            .map_err(|e| e.to_string())?;
        Ok(handle.for_agent(id))
    }

    /// Tells the user once per model that a child runs on derived facts. The main model and the
    /// live guardian announce themselves, so a child on either stays quiet.
    fn warn_derived(&self, model: &ModelHandle) {
        let main = read(&self.main, "main model").handle.reference.clone();
        warn_derived_facts(
            model,
            &main,
            self.state.guardian().label(),
            self.state.agents(),
            &*self.approver,
        );
    }
}

fn warn_derived_facts(
    model: &ModelHandle,
    main: &ModelRef,
    guardian_label: &str,
    tree: &AgentTree,
    approver: &dyn Approver,
) {
    let Some(notice) = &model.facts_notice else {
        return;
    };
    let reference = model.reference.to_string();
    if model.reference != *main
        && guardian_label != reference
        && tree.first_facts_notice(&reference)
    {
        approver.warn(notice);
    }
}

/// What one child keeps across its runs: the agent with its conversation, and how to start
/// the next run.
struct RunShared {
    id: String,
    state: Arc<SessionState>,
    steering: Steering,
    results: Arc<ResultPipeline>,
    approver: Arc<dyn Approver>,
    /// Swapped when a quota failure moves the child to the next entry of its list; each drive
    /// clones the current one.
    agent: Mutex<Arc<Agent>>,
    model: Mutex<ChildModel>,
    agent_type: AgentType,
    /// What a rebuild on another entry needs again.
    context: Arc<ChildContext>,
    tools: ToolServerHandle,
    budget: fontaine_agents::budget::Budget,
    /// The list the child is served from now: its own until a spent usage window moves it to the
    /// `fallback` role's, and it stays there.
    list: Mutex<Option<Arc<ChildList>>>,
    kill: watch::Sender<bool>,
    /// The current run's task, which only a final exit aborts: a kill waits for the run to
    /// reach a hook boundary and save its transcript.
    task: Mutex<Option<tokio::task::AbortHandle>>,
    status: Mutex<ChildStatus>,
    /// The ordinal of the latest run, and its report once it has ended.
    reports: watch::Sender<(u64, Option<ChildReport>)>,
}

impl RunShared {
    fn list(&self) -> Option<Arc<ChildList>> {
        lock(&self.list, "child model list").clone()
    }

    fn model(&self) -> ChildModel {
        lock(&self.model, "child model").clone()
    }

    fn agent(&self) -> Arc<Agent> {
        lock(&self.agent, "child agent").clone()
    }
}

/// Records what the human said to a child and shows it in the child's view.
fn record_human_prompt(shared: &RunShared, text: &str) {
    shared.state.append(Record::UserMessage {
        text: text.to_string(),
        attachments: Vec::new(),
        agent: Some(shared.id.clone()),
    });
    shared
        .state
        .emit_agent_event(&shared.id, AgentViewEvent::Prompt(text.to_string()));
}

/// A running child: its status, its kill switch and its report.
struct ChildRun {
    shared: Arc<RunShared>,
}

/// Starts one run of the child in the background; the run records its own end. The span closes
/// with the run, so nothing but the task holds it.
fn launch(shared: Arc<RunShared>, prompt: String, span: tracing::Span) {
    let kill = shared.kill.subscribe();
    let task_slot = shared.clone();
    let task = tokio::spawn(
        async move {
            let report = run_child(&shared, prompt, kill).await;
            *lock(&shared.status, "child status") = report.status;
            shared
                .reports
                .send_modify(|(_, latest)| *latest = Some(report));
        }
        .instrument(span),
    );
    let handle = task.abort_handle();
    *lock(&task_slot.task, "child task") = Some(handle.clone());
    // `abort_all` latches before it collects handles, so a run that started after it collected
    // none sees the latch here.
    if task_slot.state.agents().aborted() {
        let _ = task_slot.kill.send(true);
        handle.abort();
    }
}

impl ChildRun {
    /// Starts another run on `text`. The human's own words are recorded as theirs, which is what
    /// makes them intent for the guardian; anything else (a peer's message, a hand-off note, a
    /// hook's reason) is only shown in the child's view.
    fn start_run(&self, text: String, from_human: bool) {
        let shared = &self.shared;
        // `Node::live_status` reads this while the node runs; `launch` writes it only at run end.
        *lock(&shared.status, "child status") = ChildStatus::Running;
        if from_human {
            record_human_prompt(shared, &text);
        } else {
            shared
                .state
                .emit_agent_event(&shared.id, AgentViewEvent::Prompt(text.clone()));
        }
        shared.reports.send_modify(|(run, latest)| {
            *run += 1;
            *latest = None;
        });
        let span = tracing::info_span!(
            target: TARGET,
            "fontaine.subagent",
            agent.id = shared.id.as_str(),
            agent.type = shared.agent_type.name.as_str(),
            model.priced = shared.model().priced,
            resumed = true,
        );
        launch(shared.clone(), text, span);
    }
}

impl ChildHandle for ChildRun {
    fn status(&self) -> ChildStatus {
        let status = *lock(&self.shared.status, "child status");
        if status == ChildStatus::Running && self.shared.state.is_waiting(&self.shared.id) {
            ChildStatus::WaitingOnPermission
        } else {
            status
        }
    }

    fn spend(&self) -> Spend {
        self.shared.state.agents().spend_of(&self.shared.id)
    }

    fn kill(&self) {
        let _ = self.shared.kill.send(true);
    }

    fn abort(&self) {
        let _ = self.shared.kill.send(true);
        if let Some(task) = lock(&self.shared.task, "child task").take() {
            task.abort();
        }
    }

    fn report(&self) -> BoxFuture<'_, ChildReport> {
        let mut reports = self.shared.reports.subscribe();
        let run = reports.borrow().0;
        Box::pin(async move {
            loop {
                let latest = {
                    let seen = reports.borrow();
                    seen.1.clone().filter(|_| seen.0 >= run)
                };
                if let Some(done) = latest {
                    return done;
                }
                if reports.changed().await.is_err() {
                    return ChildReport {
                        status: ChildStatus::Failed,
                        text: String::new(),
                        reason: Some("the subagent's task ended without a report".into()),
                    };
                }
            }
        })
    }

    fn resume(&self, prompt: String) {
        self.start_run(prompt, true);
    }

    fn resume_with_data(&self, text: String) {
        self.start_run(text, false);
    }

    fn take_missed(&self) -> Vec<String> {
        self.shared.steering.take_pending_for(&self.shared.id)
    }

    fn report_undelivered(&self, refusal: &str, text: &str) {
        self.shared.approver.warn(&format!(
            "{} could not read your message ({refusal}); it was not delivered: {text}",
            self.shared.id
        ));
    }
}

/// A child built and not yet running.
struct Assembled {
    shared: Arc<RunShared>,
    span: tracing::Span,
}

/// What the tree, the budgets and the child's turns need to know about the model it runs on, and
/// the tree's record of it.
fn child_model(
    context: &ChildContext,
    id: &str,
    agent_type: &AgentType,
    model: &ModelHandle,
    served: Served,
    budget: &fontaine_agents::budget::Budget,
) -> ChildModel {
    context
        .state
        .agents()
        .set_model(id, model.reference.to_string());
    if budget.cost_usd.is_some() && !model.priced() && context.state.agents().first_unpriced_cap() {
        let token_note = if budget.tokens.is_some() {
            "; the token budget still applies"
        } else {
            ""
        };
        context.approver.notice(&format!(
            "the cost budget for subagents is inactive on {}, which has no known price{token_note}",
            model.reference
        ));
    }
    // A type's own level, else the session's. Effort is not a permission, so a child
    // may run above or below its parent.
    let effort = agent_type.effort.or(context.state.effort());
    context.state.agents().set_effort(id, effort);
    let WireIds {
        provider,
        model: model_id,
    } = model.wire_ids();
    ChildModel {
        priced: model.catalog_billed(),
        model: model_id,
        provider,
        generation_options: model.generation_options_warned(&context.state, effort),
        max_tokens: model.turn_max_tokens(context.max_output_tokens),
        quota: model.quota.clone(),
        modalities: model
            .completion
            .spec()
            .map_or_else(rig_core::catalog::Modalities::default, |spec| spec.input),
        served,
    }
}

/// The agent of child `id` on `model`, over the child's own log. `kill` is the receiver the kill
/// hook watches; the choke point's registry holds a receiver of the same channel.
fn build_child_agent(
    context: &ChildContext,
    id: &str,
    agent_type: &AgentType,
    tools: &ToolServerHandle,
    model: &ModelHandle,
    kill: watch::Receiver<bool>,
) -> Agent {
    let state = &context.state;
    let store = context.store.clone();
    let mut preamble = (context.preamble)(agent_type);
    if let Some(section) = child_section(&context.deferral, tools) {
        preamble.push_str("\n\n");
        preamble.push_str(&section);
    }
    let builder = AgentBuilder::new(model.completion.clone())
        .preamble(preamble)
        .memory(store.clone())
        .conversation(id.to_string());
    apply_common(builder, state, context.record_content)
        .add_hook(SteeringHook::new(context.steering.clone(), state.clone()))
        .add_hook(KillHook { kill })
        .add_hook(DeferredAdvertiser {
            agent: id.to_string(),
            tools: tools.clone(),
            deferral: context.deferral.clone(),
            loaded: context.loaded.clone(),
        })
        .add_hook(RecoveryHook::new(
            store.clone(),
            id.to_string(),
            context.approver.clone(),
        ))
        .add_hook(InflightHook::new(
            store,
            id.to_string(),
            context.approver.clone(),
        ))
        .tool_server_handle(tools.clone())
        .build()
}

fn served_of(list: Option<&ChildList>, model: &ModelHandle) -> Served {
    Served::new(
        list.and_then(|list| list.chain.role.as_deref()),
        &model.reference,
    )
}

impl CoreChildRunner {
    /// Builds the agent of child `id` over its own log: its model, its tool set and its hooks.
    /// `seed_fork` copies the parent's conversation in first, for a type that inherits context.
    async fn assemble(
        &self,
        id: &str,
        agent_type: &AgentType,
        model_spec: &ModelSpec,
        tools: &[String],
        budget: fontaine_agents::budget::Budget,
        seed_fork: bool,
    ) -> Result<Assembled, String> {
        let context = &self.context;
        let Resolved {
            handle: model,
            list,
        } = context.model_for(model_spec, id).await?;
        context.warn_derived(&model);
        let store = context.store.clone();
        if seed_fork {
            context
                .parent_context
                .seed(&store, id)
                .await
                .map_err(|e| format!("could not fork the conversation: {}", chain(&e)))?;
        }
        let served = served_of(list.as_ref(), &model);
        let child = child_model(context, id, agent_type, &model, served, &budget);
        let priced = child.priced;
        let tools = fork_tools(&context.tool_server, tools);
        // A child loads only the deferred tools its own toolset holds, and starts with none.
        let held = tools.clone();
        context.loaded.enroll(
            id,
            Arc::new(move |name| held.snapshot().names().any(|held| held == name)),
        );
        // The channel lives above the agent build so the hook can subscribe; the choke
        // point's registry takes the same receiver.
        let (kill, kill_rx) = watch::channel(false);
        context.kills.register(id, kill_rx.clone());
        let agent = build_child_agent(context, id, agent_type, &tools, &model, kill_rx);
        context
            .state
            .set_child_restrictions(id, restrictions(agent_type));

        let span = tracing::info_span!(
            target: TARGET,
            "fontaine.subagent",
            agent.id = id,
            agent.type = agent_type.name.as_str(),
            model.priced = priced,
        );
        let (reports, _) = watch::channel((0, None));
        let shared = Arc::new(RunShared {
            id: id.to_string(),
            state: context.state.clone(),
            steering: context.steering.clone(),
            results: context.results.clone(),
            approver: context.approver.clone(),
            agent: Mutex::new(Arc::new(agent)),
            model: Mutex::new(child),
            agent_type: agent_type.clone(),
            context: context.clone(),
            tools,
            budget,
            list: Mutex::new(list.map(Arc::new)),
            kill,
            task: Mutex::new(None),
            status: Mutex::new(ChildStatus::Running),
            reports,
        });
        Ok(Assembled { shared, span })
    }
}

impl ChildRunner for CoreChildRunner {
    fn spawn<'a>(
        &'a self,
        request: SpawnRequest,
    ) -> BoxFuture<'a, Result<Arc<dyn ChildHandle>, String>> {
        Box::pin(async move {
            let forked = request.agent_type.inherits_context;
            let Assembled { shared, span } = self
                .assemble(
                    &request.id,
                    &request.agent_type,
                    &request.model,
                    &request.tools,
                    request.budget,
                    forked,
                )
                .await?;
            let state = &self.context.state;
            let handle = Arc::new(ChildRun {
                shared: shared.clone(),
            });

            // The view opens on the task as the parent gave it, without the fork note.
            state.emit_agent_event(&request.id, AgentViewEvent::Prompt(request.prompt.clone()));
            let prompt = if forked {
                format!("{FORK_NOTE}\n\n{}", request.prompt)
            } else {
                request.prompt
            };
            // Ahead of the task, as notes the parent did not write.
            let notes = state
                .agents()
                .lifecycle()
                .subagent_start(&request.id, &request.agent_type.name)
                .await;
            let prompt = if notes.is_empty() {
                prompt
            } else {
                format!("{}\n\n{prompt}", notes.join("\n\n"))
            };
            launch(shared, prompt, span);
            Ok(handle as Arc<dyn ChildHandle>)
        })
    }

    fn discard_unread(&self, id: &str) {
        self.context.steering.take_pending_for(id);
    }

    fn revive<'a>(
        &'a self,
        request: RevivalRequest,
    ) -> BoxFuture<'a, Result<Arc<dyn ChildHandle>, String>> {
        Box::pin(async move {
            // `rig_memory::load` reads a missing log as an empty conversation, which would
            // revive an agent that remembers nothing.
            let logs = self.context.store.logs().map_err(|e| {
                format!("the saved conversations could not be listed: {}", chain(&e))
            })?;
            if !logs.contains(&request.id) {
                return Err(format!(
                    "its saved conversation {}.jsonl is missing",
                    request.id
                ));
            }
            // The recorded reference is concrete; the session's own model needs no lookup.
            let model = match &request.model {
                ModelSpec::Model(text)
                    if *text
                        == read(&self.context.main, "main model")
                            .handle
                            .reference
                            .to_string() =>
                {
                    ModelSpec::Main
                }
                other => other.clone(),
            };
            let Assembled { shared, .. } = self
                .assemble(
                    &request.id,
                    &request.agent_type,
                    &model,
                    &request.tools,
                    request.budget,
                    false,
                )
                .await?;
            // Nothing runs yet: the first wake starts it, which sets the status again.
            *lock(&shared.status, "child status") = ChildStatus::Done;
            Ok(Arc::new(ChildRun { shared }) as Arc<dyn ChildHandle>)
        })
    }
}

/// What a child's model call reports to usage and error telemetry.
#[derive(Clone)]
struct ChildModel {
    priced: bool,
    model: String,
    provider: String,
    /// The reasoning effort the child's turns send, validated against its model at spawn.
    generation_options: Option<rig_core::completion::GenerationOptions>,
    /// The output cap the child's turns send, from its own model and the session's cap.
    max_tokens: Option<u64>,
    /// What the child's model accepts, for the read tool's multimodal dispatch.
    modalities: rig_core::catalog::Modalities,
    /// The role and model its calls are recorded against.
    served: Served,
    /// Where its account's own usage figures are read, when it opted in.
    quota: Option<Arc<dyn crate::quota::QuotaSource>>,
}

/// How a child's wait for its own subagents ended.
enum Joined {
    /// None runs and nothing is waiting for the child.
    Done,
    /// News or the human's words for a further turn.
    Prompt(String),
}

/// Waits until the child has something to act on or none of its subagents runs, checking in the
/// order a headless root does. The human's typing never notifies the tree, so the wait polls.
async fn join_wait(shared: &RunShared) -> Joined {
    let tree = shared.state.agents().clone();
    let id = shared.id.as_str();
    loop {
        let changed = tree.changed().notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if let Some(joined) = join_news(shared).await {
            return joined;
        }
        if !tree.running_below(id) {
            // A report is queued before its child is marked finished, so nothing is missed
            // between the check above and this one.
            return join_news(shared).await.unwrap_or(Joined::Done);
        }
        tokio::select! {
            _ = &mut changed => {}
            _ = tokio::time::sleep(WAKE_POLL) => {}
        }
    }
}

async fn join_news(shared: &RunShared) -> Option<Joined> {
    let id = shared.id.as_str();
    if shared.state.agents().has_notifications(id)
        && let Some(prompt) = notification_prompt(&shared.state, &shared.results, id).await
    {
        shared
            .state
            .emit_agent_event(id, AgentViewEvent::Prompt(prompt.clone()));
        return Some(Joined::Prompt(prompt));
    }
    if shared.steering.has_deliverable_message_for(id) {
        let typed = shared.steering.take_pending_for(id);
        if !typed.is_empty() {
            let prompt = typed.join("\n\n");
            record_human_prompt(shared, &prompt);
            return Some(Joined::Prompt(prompt));
        }
    }
    None
}

/// Runs one child to its end: an answer once its own subagents have reported, a spent budget, a
/// kill or a failure.
async fn run_child(
    shared: &RunShared,
    prompt: String,
    mut kill: watch::Receiver<bool>,
) -> ChildReport {
    let state = &shared.state;
    let id = shared.id.as_str();
    let tree = state.agents().clone();
    let last_text = Arc::new(Mutex::new(String::new()));
    let mut prompt = prompt;
    // The entries of its list the child's current request has failed on.
    let mut tried = Tried::default();
    // The accounts whose usage window the child has hit since it last waited one out; kept apart
    // from `tried` because a list-less child has no list to note them on.
    let mut spent = BTreeSet::new();
    let outcome = loop {
        let seen = last_text.clone();
        let view_state = state.clone();
        let view_id = id.to_string();
        let budget_tree = tree.clone();
        let mut on_event = move |event: SessionEvent| -> Flow {
            if let SessionEvent::Assistant(text) = &event {
                // Partial text only labels an interrupted child's report; a stale value is harmless.
                *lock_recovering(&seen) = text.clone();
            }
            view_state.emit_agent_event(&view_id, AgentViewEvent::Event(event));
            // A spent budget ends the run at the next event: the guardian's calls for this child
            // count against it too.
            if budget_tree.exhausted_of(&view_id).is_some() {
                Flow::Stop
            } else {
                Flow::Continue
            }
        };
        let model = shared.model();
        let agent = shared.agent();
        let run = drive(
            Drive {
                agent: &agent,
                state,
                agent_id: id,
                prompt,
                attachments: Vec::new(),
                modalities: model.modalities,
                max_turns: None,
                priced: model.priced,
                model: model.model.clone(),
                provider: model.provider.clone(),
                native_search: false,
                generation_options: model.generation_options.clone(),
                max_tokens: model.max_tokens,
                served: model.served.clone(),
                span: tracing::Span::current(),
            },
            None,
            &mut on_event,
        );
        tokio::pin!(run);
        let mut outcome = tokio::select! {
            biased;
            // `map` keeps the flag wait's borrowed value out of the arm, so the grace wait
            // holds no watch guard across its await.
            _ = kill.wait_for(|killed| *killed).map(|_| ()) => {
                // The kill hook settles the run at the next hook boundary, normally well inside
                // the grace; only a wedged run (a hung stream, a stuck hook) outlives it and is
                // dropped exactly as an instantaneous kill drops it today.
                match tokio::time::timeout(KILL_GRACE, &mut run).await {
                    Ok(outcome) => break Some(outcome),
                    Err(_) => break None,
                }
            }
            outcome = &mut run => outcome,
        };
        if let TurnOutcome::Failed(failure) = &outcome
            && failure.falls_through
            && !*kill.borrow()
        {
            let mut failure = failure.clone();
            if failure.usage_limit.is_some() {
                spent.insert(shared.model().served.account().to_string());
            }
            match fall_through_child(shared, &failure, &mut tried, &spent).await {
                Ok(()) => {
                    prompt = RETRY_NOTE.to_string();
                    continue;
                }
                Err(spent) if !spent.is_empty() => {
                    failure.message = format!("{}\n{spent}", failure.message);
                }
                Err(_) => {}
            }
            // Nothing left to try on a spent usage window: the whole run waits for it, the
            // child included, and a kill ends the wait.
            if failure.usage_limit.is_some() {
                if fall_back_child(shared, &failure, &mut tried, &spent)
                    .await
                    .is_ok()
                {
                    prompt = RETRY_NOTE.to_string();
                    continue;
                }
                let model = shared.model();
                let account = model.served.account().to_string();
                let waited = tokio::select! {
                    biased;
                    _ = kill.wait_for(|killed| *killed) => None,
                    waited = wait_out_usage_limit(
                        state,
                        &mut failure,
                        UsageRole::Main,
                        &account,
                        model.quota.clone(),
                    ) => Some(waited),
                };
                match waited {
                    Some(true) => {
                        tried = Tried::default();
                        spent.clear();
                        prompt = RETRY_NOTE.to_string();
                        continue;
                    }
                    Some(false) => {}
                    None => break None,
                }
            }
            outcome = TurnOutcome::Failed(failure);
        }
        let TurnOutcome::Answer(_) = outcome else {
            break Some(outcome);
        };
        tried = Tried::default();
        // Finishing would kill the child's running subagents, so it ends only once they report.
        let joined = tokio::select! {
            biased;
            _ = kill.wait_for(|killed| *killed) => break None,
            joined = join_wait(shared) => joined,
        };
        match joined {
            Joined::Done => break Some(outcome),
            Joined::Prompt(next) => prompt = next,
        }
    };
    // Partial text only labels an interrupted child's report; a stale value is harmless.
    let partial = lock_recovering(&last_text).clone();
    // A temperature stop ends every agent's run at its next dispatch, which can come before the
    // surface's stop handler kills the tree; the stop is the kill either way.
    let killed = *kill.borrow() || state.afk().stopped.is_some();
    child_report(&tree, id, outcome, partial, killed)
}

/// Moves a child whose model ran out of quota to the next entry of its list that can serve,
/// rebuilds its agent on it and says so. On a role's or an inline list's chain the entry is
/// settled for every other child on it; a child on `$main` moves for itself. `Err` is what to
/// add to the failure when nothing could serve, empty when the child has no list to move on.
/// On the `fallback` role's list, entries on `spent` accounts are skipped too.
async fn fall_through_child(
    shared: &RunShared,
    failure: &TurnFailure,
    tried: &mut Tried,
    spent: &BTreeSet<String>,
) -> Result<(), String> {
    let Some(list) = shared.list() else {
        return Err(String::new());
    };
    let failed = list.index.load(Ordering::Acquire);
    let from = list.chain.entries[failed].clone();
    let order = tried.order(&list.chain, failed);
    let none = BTreeSet::new();
    let skip = if on_fallback(&list) { spent } else { &none };
    let moved = switch_child(
        shared,
        &list,
        order,
        tried,
        skip,
        &from.to_string(),
        failure.class.unwrap_or("quota"),
        |to| fall_through_notice(&shared.id, &from, to, failure.class.unwrap_or("quota")),
    )
    .await;
    if moved {
        return Ok(());
    }
    Err(if list.chain.entries.len() > 1 {
        format!("every model in {} failed", list.chain.label())
    } else {
        String::new()
    })
}

/// Moves a subagent whose own list is spent on a usage window to the first usable entry of the
/// `fallback` role, rebuilds its agent on it and says so; it serves from that list from then on.
/// No shared chain is settled, so the role's other subagents keep their entry. `Err` when no
/// fallback is defined, the child is already on it, or none of its entries can serve.
async fn fall_back_child(
    shared: &RunShared,
    failure: &TurnFailure,
    tried: &mut Tried,
    spent: &BTreeSet<String>,
) -> Result<(), String> {
    let context = &shared.context;
    let Some(chain) = context.chains.chain_of_role(&context.roles, FALLBACK_ROLE) else {
        return Err(String::new());
    };
    let current = shared.list();
    if current.as_deref().is_some_and(on_fallback) {
        return Err(String::new());
    }
    let from = match &current {
        Some(list) => list.chain.entries[list.index.load(Ordering::Acquire)].to_string(),
        None => shared.model().served.model,
    };
    // Every skipped entry is noted below, since the unsettled chain stays on entry 0 and a later
    // fall-through would otherwise walk back onto a spent account.
    *tried = Tried::default();
    let list = Arc::new(ChildList {
        index: AtomicUsize::new(0),
        settles: false,
        chain,
    });
    let order = 0..list.chain.entries.len();
    let moved = switch_child(
        shared,
        &list,
        order,
        tried,
        spent,
        &from,
        failure.class.unwrap_or("quota"),
        |to| fall_back_notice(&shared.id, &from, to),
    )
    .await;
    if !moved {
        return Err(String::new());
    }
    *lock(&shared.list, "child model list") = Some(list);
    Ok(())
}

fn on_fallback(list: &ChildList) -> bool {
    // Roles share one chain per name, so the role names the fallback chain exactly.
    list.chain.role.as_deref() == Some(FALLBACK_ROLE)
}

/// Rebuilds the child on the first entry of `order` that resolves, fits an explicit budget cap and
/// is not on a `spent` account, noting every entry it passes over. `from` is what the child leaves,
/// for the record. False when no entry could serve.
#[allow(clippy::too_many_arguments)]
async fn switch_child(
    shared: &RunShared,
    list: &ChildList,
    order: impl IntoIterator<Item = usize>,
    tried: &mut Tried,
    spent: &BTreeSet<String>,
    from: &str,
    class: &str,
    notice: impl FnOnce(&ModelRef) -> String,
) -> bool {
    let context = &shared.context;
    for index in order {
        let reference = &list.chain.entries[index];
        if spent.contains(&reference.account) {
            tried.note(index);
            continue;
        }
        let handle = match context.entry_for(reference, &shared.id).await {
            Ok(handle) => handle,
            Err(why) => {
                shared
                    .approver
                    .warn(&format!("{}: skipping {reference}: {why}", shared.id));
                tried.note(index);
                continue;
            }
        };
        if context.explicit_budget_cap && !handle.priced() {
            tried.note(index);
            continue;
        }
        if list.settles {
            list.chain.settle(index);
        }
        list.index.store(index, Ordering::Release);
        tried.note(index);
        let served = served_of(Some(list), &handle);
        let model = child_model(
            context,
            &shared.id,
            &shared.agent_type,
            &handle,
            served,
            &shared.budget,
        );
        let agent = build_child_agent(
            context,
            &shared.id,
            &shared.agent_type,
            &shared.tools,
            &handle,
            shared.kill.subscribe(),
        );
        *lock(&shared.agent, "child agent") = Arc::new(agent);
        *lock(&shared.model, "child model") = model;
        context.state.record_model_fall_through(
            &shared.id,
            list.chain.role.as_deref(),
            from,
            &reference.to_string(),
            class,
        );
        context.approver.warn(&notice(reference));
        return true;
    }
    false
}

/// How a child's run reads once it has settled. A kill that fired first settles the report as
/// `Killed` whatever the outcome was, including an answer that completed concurrently with the
/// kill; the tree already showed the node `Killed` the moment the human stopped it.
fn child_report(
    tree: &AgentTree,
    id: &str,
    outcome: Option<TurnOutcome>,
    partial: String,
    killed: bool,
) -> ChildReport {
    if killed || outcome.is_none() {
        return ChildReport {
            status: ChildStatus::Killed,
            text: partial,
            reason: Some(KILL_REASON.into()),
        };
    }
    match outcome.expect("a live outcome when the kill flag is clear") {
        TurnOutcome::Answer(text) => ChildReport {
            status: ChildStatus::Done,
            text,
            reason: None,
        },
        TurnOutcome::Stopped => ChildReport {
            status: ChildStatus::BudgetExhausted,
            text: partial,
            reason: tree.exhausted_of(id).map(|why| why.describe()),
        },
        TurnOutcome::MaxTurns(_) => ChildReport {
            status: ChildStatus::BudgetExhausted,
            text: partial,
            reason: Some("it reached the limit on model calls".into()),
        },
        TurnOutcome::Failed(failure) => ChildReport {
            status: ChildStatus::Failed,
            text: partial,
            reason: Some(failure.message),
        },
    }
}

/// A usage report in the tree's terms; re-exported for the surfaces.
pub fn spend(usage: &rig_core::completion::Usage) -> Spend {
    spend_of(usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steering::Queued;
    use fontaine_agents::mode::ModeSetting;
    use fontaine_agents::registry::TypeOrigin;

    fn typed(tools: &[&str]) -> AgentType {
        AgentType {
            name: "t".to_string(),
            description: String::new(),
            prompt: None,
            tools: Some(tools.iter().map(|t| t.to_string()).collect()),
            disallowed: Vec::new(),
            model: None,
            mode: ModeSetting::Inherit,
            effort: None,
            origin: TypeOrigin::Builtin,
            inherits_context: false,
        }
    }

    #[derive(Default)]
    struct Recording(Mutex<Vec<String>>);

    impl Approver for Recording {
        fn approve<'a>(
            &'a self,
            _request: crate::permissions::ApprovalRequest,
        ) -> futures::future::BoxFuture<'a, crate::permissions::Answer> {
            unreachable!("nothing asks for approval")
        }

        fn warn(&self, message: &str) {
            self.0.lock().unwrap().push(message.to_string());
        }
    }

    fn derived(reference: &str) -> ModelHandle {
        let mut handle = ModelHandle::new(
            ModelRef::parse(reference).unwrap(),
            crate::config::AccountKind::named("claude-subscription"),
            rig_core::test_utils::MockCompletionModel::default().into(),
        );
        handle.facts_notice = Some(format!("{reference} is derived"));
        handle
    }

    #[test]
    fn a_derived_child_model_warns_once_and_main_and_guardian_stay_quiet() {
        let tree = AgentTree::new(fontaine_agents::tree::Limits::default(), 1);
        let approver = Recording::default();
        let main = ModelRef::parse("claude/sonnet").unwrap();
        let warn = |reference: &str| {
            warn_derived_facts(&derived(reference), &main, "claude/opus", &tree, &approver);
        };
        warn("claude/haiku");
        warn("claude/haiku");
        warn("claude/fable");
        warn("claude/sonnet");
        warn("claude/opus");
        let told = approver.0.lock().unwrap().clone();
        assert_eq!(told, ["claude/haiku is derived", "claude/fable is derived"]);
        let listed = ModelHandle::new(
            ModelRef::parse("claude/other").unwrap(),
            crate::config::AccountKind::named("claude-subscription"),
            rig_core::test_utils::MockCompletionModel::default().into(),
        );
        warn_derived_facts(&listed, &main, "claude/opus", &tree, &approver);
        assert_eq!(approver.0.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_tool_listed_bare_is_not_limited_by_its_specifier() {
        let only = restrictions(&typed(&["shell", "shell(git *)", "read(src/*)"])).only;
        let tools: Vec<&str> = only.iter().map(|r| r.tool.as_str()).collect();
        assert_eq!(tools, ["read"]);
    }

    fn kill_hook(flag: bool) -> KillHook {
        let (tx, rx) = watch::channel(flag);
        drop(tx);
        KillHook { kill: rx }
    }

    #[test]
    fn the_kill_hook_proceeds_with_the_flag_clear() {
        let hook = kill_hook(false);
        assert_eq!(hook.completion_call(), CompletionCallAction::Continue);
        assert!(matches!(hook.dispatch(true), DispatchAction::Proceed));
    }

    #[test]
    fn the_kill_hook_stops_the_run_with_the_flag_set() {
        let hook = kill_hook(true);
        assert_eq!(
            hook.completion_call(),
            CompletionCallAction::Stop(KILL_REASON.to_string())
        );
        let stopped = hook.dispatch(true);
        let DispatchAction::Deny(report) = stopped else {
            panic!("a killed tool call is refused, not dispatched: {stopped:?}");
        };
        assert_eq!(report.message, KILL_REASON);
        // A non-tool effect is not a run-stopping tool denial; it proceeds.
        assert!(matches!(hook.dispatch(false), DispatchAction::Proceed));
    }

    fn tree() -> Arc<AgentTree> {
        Arc::new(AgentTree::new(fontaine_agents::tree::Limits::default(), 1))
    }

    #[test]
    fn a_killed_run_reports_killed_whatever_it_settled_as() {
        let tree = tree();
        // A run that settled after the kill fired, as a failure from the cancel.
        let report = child_report(
            &tree,
            "agent-1",
            Some(TurnOutcome::Failed("the run was cancelled".into())),
            String::new(),
            true,
        );
        assert_eq!(report.status, ChildStatus::Killed);
        assert_eq!(report.reason.as_deref(), Some(KILL_REASON));
        // An answer that completed concurrently with the kill is still a kill.
        let report = child_report(
            &tree,
            "agent-1",
            Some(TurnOutcome::Answer("too late".into())),
            "partial".into(),
            true,
        );
        assert_eq!(report.status, ChildStatus::Killed);
        assert_eq!(report.text, "partial");
        // A dropped, never-settled run is a kill.
        let report = child_report(&tree, "agent-1", None, String::new(), true);
        assert_eq!(report.status, ChildStatus::Killed);
    }

    #[test]
    fn a_run_without_the_kill_flag_keeps_its_own_outcome() {
        let tree = tree();
        let report = child_report(
            &tree,
            "agent-1",
            Some(TurnOutcome::Answer("done".into())),
            String::new(),
            false,
        );
        assert_eq!(report.status, ChildStatus::Done);
        assert_eq!(report.text, "done");
        let report = child_report(
            &tree,
            "agent-1",
            Some(TurnOutcome::Failed("provider error".into())),
            "partial".into(),
            false,
        );
        assert_eq!(report.status, ChildStatus::Failed);
        assert_eq!(report.text, "partial");
    }

    #[test]
    fn a_message_the_human_typed_to_a_child_wakes_its_wait_and_no_other() {
        let tree = Arc::new(AgentTree::new(fontaine_agents::tree::Limits::default(), 1));
        let steering = Steering::default();
        let wake = CoreWake {
            tree,
            steering: steering.clone(),
        };
        assert!(!wake.pending("agent-1"));
        steering.queue_for("agent-1", "wake up".into());
        assert!(wake.pending("agent-1"));
        assert!(!wake.pending("agent-2"));
        assert!(!wake.pending(ROOT_AGENT));
    }

    #[test]
    fn a_queued_command_wakes_the_root_wait_once_and_a_lone_message_has_no_notice() {
        let tree = Arc::new(AgentTree::new(fontaine_agents::tree::Limits::default(), 1));
        let steering = Steering::default();
        let wake = CoreWake {
            tree,
            steering: steering.clone(),
        };
        assert!(!wake.pending(ROOT_AGENT));
        steering.queue(Queued::Command("/afk".into()));
        assert!(wake.pending(ROOT_AGENT));
        assert!(!wake.pending("agent-1"));
        assert!(
            wake.take_notice(ROOT_AGENT)
                .is_some_and(|n| n.contains("/afk"))
        );
        assert!(!wake.pending(ROOT_AGENT));

        steering.retract();
        steering.queue(Queued::Message("hi".into()));
        assert!(wake.pending(ROOT_AGENT));
        assert_eq!(wake.take_notice(ROOT_AGENT), None);
    }
}
