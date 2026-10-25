//! Running subagents. The `agent` tool, the registry and the budgets live in `fontaine-agents`;
//! this is what starts a child there: its own model, its own context, a tool set cut from the
//! parent's, and every call it makes through the same permission pipeline as everyone's.

use std::sync::{Arc, Mutex, RwLock};

use fontaine_agents::budget::Spend;
use fontaine_agents::registry::{AgentType, ModelSpec, entry_has_specifier};
use fontaine_agents::root_ledger::{AgentEvent, RootLedger};
use fontaine_agents::runner::{ChildHandle, ChildReport, ChildRunner, ChildStatus, SpawnRequest};
use fontaine_agents::tree::AgentTree;
use fontaine_guardian::WireIds;
use fontaine_tools::gate::CallView;
use fontaine_tools::lock::{lock, lock_recovering, read};
use fontaine_tools::result_pipeline::{ResultStage, ToolOutcome, append_note};
use futures::future::BoxFuture;
use rig_agent::prelude::*;
use rig_agent::tool::server::{ToolServer, ToolServerHandle};
use rig_memory::MemoryStore;
use tokio::sync::watch;
use tracing::Instrument;

use crate::agent::apply_common;
use crate::gate::{ChildRestrictions, SessionState, spend_of};
use crate::ledger::Record;
use crate::models::{ModelHandle, SharedSwitcher};
use crate::permission_rules::parse_rule;
use crate::permissions::Approver;
use crate::turn::{DEFAULT_MAX_TURNS, Drive, Flow, SessionEvent, TurnOutcome, drive};

const TARGET: &str = "fontaine::telemetry";

/// The main model as children see it: what `$main` means, and the listing that vouches for a
/// model on its account.
#[derive(Clone)]
pub struct MainModel {
    pub handle: ModelHandle,
    pub listing: Vec<String>,
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
                parent,
                agent_type,
            } => Record::Agent {
                event: "spawned".into(),
                agent,
                parent: Some(parent),
                detail: Some(agent_type),
            },
            AgentEvent::Finished { agent, status } => Record::Agent {
                event: "finished".into(),
                agent,
                parent: None,
                detail: Some(status),
            },
            AgentEvent::Message { from, to } => Record::Agent {
                event: "message".into(),
                agent: from,
                parent: None,
                detail: Some(to),
            },
        };
        self.0.append(record);
    }
}

type Preamble = Arc<dyn Fn(&AgentType) -> String + Send + Sync>;

/// What starting a child needs from the session.
pub struct ChildContext {
    pub state: Arc<SessionState>,
    pub tool_server: ToolServerHandle,
    pub main: Arc<RwLock<MainModel>>,
    pub switcher: Option<SharedSwitcher>,
    pub preamble: Preamble,
    pub approver: Arc<dyn Approver>,
    pub record_content: bool,
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

fn restrictions(agent_type: &AgentType) -> ChildRestrictions {
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

impl ChildContext {
    async fn model_for(&self, spec: &ModelSpec) -> Result<ModelHandle, String> {
        let main = read(&self.main, "main model").clone();
        // Cloned out of the lock: resolving a model awaits.
        let switcher = self
            .switcher
            .as_ref()
            .map(|s| read(s, "model switcher").clone());
        match spec {
            ModelSpec::Main => Ok(main.handle),
            ModelSpec::GuardianDefault => Ok(switcher
                .and_then(|s| s.guardian_default(&main.handle.reference, &main.listing))
                .unwrap_or(main.handle)),
            ModelSpec::Model(text) => {
                let Some(switcher) = switcher else {
                    return Err("models cannot be resolved in this session".to_string());
                };
                switcher
                    .main(text)
                    .await
                    .map(|(handle, _)| handle)
                    .map_err(|e| e.to_string())
            }
        }
    }
}

/// A running child: its status, its kill switch and its report.
struct ChildRun {
    id: String,
    state: Arc<SessionState>,
    kill: watch::Sender<bool>,
    status: Arc<Mutex<ChildStatus>>,
    report: watch::Receiver<Option<ChildReport>>,
}

impl ChildHandle for ChildRun {
    fn status(&self) -> ChildStatus {
        let status = *lock(&self.status, "child status");
        if status == ChildStatus::Running && self.state.is_waiting(&self.id) {
            ChildStatus::WaitingOnPermission
        } else {
            status
        }
    }

    fn spend(&self) -> Spend {
        self.state.agents().spend_of(&self.id)
    }

    fn kill(&self) {
        let _ = self.kill.send(true);
    }

    fn report(&self) -> BoxFuture<'_, ChildReport> {
        let mut report = self.report.clone();
        Box::pin(async move {
            loop {
                if let Some(done) = report.borrow().clone() {
                    return done;
                }
                if report.changed().await.is_err() {
                    return report.borrow().clone().unwrap_or(ChildReport {
                        status: ChildStatus::Failed,
                        text: String::new(),
                        reason: Some("the subagent's task ended without a report".into()),
                    });
                }
            }
        })
    }
}

fn short(text: &str) -> String {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 200 {
        let cut: String = line.chars().take(199).collect();
        format!("{cut}…")
    } else {
        line
    }
}

impl ChildRunner for CoreChildRunner {
    fn spawn<'a>(
        &'a self,
        request: SpawnRequest,
    ) -> BoxFuture<'a, Result<Arc<dyn ChildHandle>, String>> {
        Box::pin(async move {
            let context = &self.context;
            let model = context.model_for(&request.model).await?;
            context
                .state
                .agents()
                .set_model(&request.id, model.reference.to_string());
            if request.budget.cost_usd.is_some()
                && !model.priced()
                && context.state.agents().first_unpriced_cap()
            {
                context.approver.notice(&format!(
                    "the cost budget for subagents is inactive on {}, which has no known price; the token budget still applies",
                    model.reference
                ));
            }
            let priced = model.catalog_billed();
            let WireIds {
                provider,
                model: model_id,
            } = model.wire_ids();
            let state = context.state.clone();
            let tools = fork_tools(&context.tool_server, &request.tools);
            let builder = AgentBuilder::new(model.completion.clone())
                .preamble((context.preamble)(&request.agent_type))
                .memory(MemoryStore::default())
                .conversation("main");
            let agent = apply_common(builder, &state, context.record_content)
                .tool_server_handle(tools)
                .build();
            state.set_child_restrictions(&request.id, restrictions(&request.agent_type));

            let span = tracing::info_span!(
                target: TARGET,
                "fontaine.subagent",
                agent.id = request.id.as_str(),
                agent.type = request.agent_type.name.as_str(),
                model.priced = priced,
            );
            let (kill, kill_rx) = watch::channel(false);
            let (report_tx, report_rx) = watch::channel(None);
            let status = Arc::new(Mutex::new(ChildStatus::Running));
            let handle = Arc::new(ChildRun {
                id: request.id.clone(),
                state: state.clone(),
                kill,
                status: status.clone(),
                report: report_rx,
            });

            let id = request.id.clone();
            let prompt = request.prompt;
            let task_state = state.clone();
            tokio::spawn(
                async move {
                    let report = run_child(
                        &task_state,
                        &agent,
                        &id,
                        prompt,
                        ChildModel {
                            priced,
                            model: model_id,
                            provider,
                        },
                        kill_rx,
                    )
                    .await;
                    *lock(&status, "child status") = report.status;
                    report_tx.send_replace(Some(report));
                }
                .instrument(span.clone()),
            );
            Ok(handle as Arc<dyn ChildHandle>)
        })
    }
}

/// What a child's model call reports to usage and error telemetry.
struct ChildModel {
    priced: bool,
    model: String,
    provider: String,
}

/// Runs one child to its end: an answer, a spent budget, a kill or a failure.
async fn run_child(
    state: &Arc<SessionState>,
    agent: &Agent,
    id: &str,
    prompt: String,
    model: ChildModel,
    mut kill: watch::Receiver<bool>,
) -> ChildReport {
    let tree = state.agents().clone();
    let last_text = Arc::new(Mutex::new(String::new()));
    let seen = last_text.clone();
    let log_state = state.clone();
    let log_id = id.to_string();
    let budget_tree = tree.clone();
    let budget_id = id.to_string();
    let mut on_event = move |event: SessionEvent| -> Flow {
        match &event {
            SessionEvent::Assistant(text) => {
                // Partial text only labels an interrupted child's report; a stale value is harmless.
                *lock_recovering(&seen) = text.clone();
                log_state.log_agent_line(&log_id, short(text));
            }
            SessionEvent::ToolCall { tool, args, .. } => {
                log_state.log_agent_line(
                    &log_id,
                    format!(
                        "→ {tool} {}",
                        short(&crate::permissions::summarize(tool, args))
                    ),
                );
            }
            SessionEvent::ToolResult { tool, success, .. } => {
                log_state.log_agent_line(
                    &log_id,
                    format!("← {tool} {}", if *success { "ok" } else { "failed" }),
                );
            }
            SessionEvent::Decision(_)
            | SessionEvent::AssistantDelta(_)
            | SessionEvent::Usage { .. } => {}
        }
        // A spent budget ends the run at the next event: the guardian's calls for this child
        // count against it too.
        if budget_tree.exhausted_of(&budget_id).is_some() {
            Flow::Stop
        } else {
            Flow::Continue
        }
    };
    let run = drive(
        Drive {
            agent,
            state,
            agent_id: id,
            prompt,
            max_turns: DEFAULT_MAX_TURNS,
            priced: model.priced,
            model: model.model,
            provider: model.provider,
            span: tracing::Span::current(),
        },
        None,
        &mut on_event,
    );
    let outcome = tokio::select! {
        biased;
        _ = kill.wait_for(|killed| *killed) => None,
        outcome = run => Some(outcome),
    };
    // Partial text only labels an interrupted child's report; a stale value is harmless.
    let partial = lock_recovering(&last_text).clone();
    match outcome {
        None => ChildReport {
            status: ChildStatus::Killed,
            text: partial,
            reason: Some("it was killed".into()),
        },
        Some(TurnOutcome::Answer(text)) => ChildReport {
            status: ChildStatus::Done,
            text,
            reason: None,
        },
        Some(TurnOutcome::Stopped) => ChildReport {
            status: ChildStatus::BudgetExhausted,
            text: partial,
            reason: tree.exhausted_of(id).map(|why| why.describe()),
        },
        Some(TurnOutcome::MaxTurns(_)) => ChildReport {
            status: ChildStatus::BudgetExhausted,
            text: partial,
            reason: Some("it reached the limit on model calls".into()),
        },
        Some(TurnOutcome::Failed(message)) => ChildReport {
            status: ChildStatus::Failed,
            text: partial,
            reason: Some(message),
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
            origin: TypeOrigin::Builtin,
        }
    }

    #[test]
    fn a_tool_listed_bare_is_not_limited_by_its_specifier() {
        let only = restrictions(&typed(&["shell", "shell(git *)", "read(src/*)"])).only;
        let tools: Vec<&str> = only.iter().map(|r| r.tool.as_str()).collect();
        assert_eq!(tools, ["read"]);
    }
}
