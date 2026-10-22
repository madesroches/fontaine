//! The two tools that make agents a model can use: `agent` starts a subagent and `send_message`
//! talks to one. Both are ordinary `Arbitrary` tools behind the permission pipeline; the
//! checks here only decide whether a request can be served at all.

use std::sync::Arc;

use fontaine_tools::gate::{AgentId, ROOT_AGENT};
use rig_core::tool::{Tool, ToolContext, ToolExecutionError, ToolOutput};
use serde::Deserialize;
use serde_json::json;

use crate::mode::own_mode;
use crate::registry::{AgentTypes, ModelSpec};
use crate::root_ledger::{AgentEvent, RootLedger};
use crate::runner::{ChildHandle, ChildReport, ChildRunner, ChildStatus, SpawnRequest};
use crate::tree::{AgentTree, DeliverError, Refusal};

const TARGET: &str = "fontaine::telemetry";

/// The longest `send_message` body.
pub const MESSAGE_CAP_CHARS: usize = 50_000;

fn caller_of(context: &ToolContext) -> String {
    context
        .get::<AgentId>()
        .ok()
        .flatten()
        .map_or_else(|| ROOT_AGENT.to_string(), |id| id.0)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentArgs {
    pub agent_type: String,
    pub prompt: String,
    pub model: Option<String>,
    pub background: Option<bool>,
}

#[derive(Clone)]
pub struct AgentTool {
    pub tree: Arc<AgentTree>,
    pub types: Arc<AgentTypes>,
    pub runner: Arc<dyn ChildRunner>,
    pub ledger: Arc<dyn RootLedger>,
}

/// Kills a child whose blocking call was dropped, such as by an interrupt.
struct KillOnDrop {
    tree: Arc<AgentTree>,
    id: String,
    armed: bool,
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.tree.kill(&self.id);
        }
    }
}

/// Gives back a reservation if the call is dropped (the turn was interrupted) before the child
/// has a handle, which would otherwise hold a parallel slot forever.
struct ReleaseOnDrop {
    tree: Arc<AgentTree>,
    id: String,
    armed: bool,
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.tree.release(&self.id);
        }
    }
}

fn subagent_event(event: &str, id: &str, agent_type: &str) {
    tracing::info!(
        target: TARGET,
        event = event,
        agent.id = id,
        agent.type = agent_type,
        "subagent"
    );
}

/// Records how a child ended: the tree, the ledger and telemetry.
fn conclude(
    tree: &AgentTree,
    ledger: &dyn RootLedger,
    id: &str,
    agent_type: &str,
    report: &ChildReport,
) {
    tree.finish(id, report.clone());
    ledger.append_event(AgentEvent::Finished {
        agent: id.to_string(),
        status: report.status.as_str().to_string(),
    });
    let event = match report.status {
        ChildStatus::Killed => "killed",
        ChildStatus::BudgetExhausted => "budget_exhausted",
        _ => "complete",
    };
    subagent_event(event, id, agent_type);
}

/// The text a parent reads for a child's report.
fn report_text(report: &ChildReport) -> String {
    match report.status {
        ChildStatus::BudgetExhausted => format!(
            "{}\n\n[Partial result: the subagent stopped because {}.]",
            report.text,
            report.reason.as_deref().unwrap_or("its budget was spent")
        ),
        _ => report.text.clone(),
    }
}

impl Tool for AgentTool {
    const NAME: &'static str = "agent";
    type Args = AgentArgs;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        format!(
            "Start a subagent: a separate conversation with its own context that works on one \
             task and returns a report. Give it everything it needs in `prompt`; it cannot see \
             this conversation. By default the call waits for the report and returns it; with \
             `background` set it returns an `agent_id` at once and the report arrives later as \
             a notification. A subagent has a subset of your tools and never more authority than \
             you, and every call it makes passes the same permission checks. Agent types:\n{}",
            self.types.describe()
        )
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "agent_type": {
                    "type": "string",
                    "enum": self.types.names(),
                    "description": "Which kind of subagent to start.",
                },
                "prompt": { "type": "string", "description": "The task, with all the context it needs." },
                "model": {
                    "type": "string",
                    "description": "Optional model for this agent: `$main`, `$guardian_default` or `<account>/<model>`. Defaults to the agent type's model.",
                },
                "background": {
                    "type": "boolean",
                    "description": "Return an agent_id at once and deliver the report later.",
                },
            },
            "required": ["agent_type", "prompt"],
            "additionalProperties": false,
        })
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: AgentArgs,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let caller = caller_of(context);
        let Some(agent_type) = self.types.get(&args.agent_type) else {
            return Err(ToolExecutionError::invalid_args(format!(
                "no agent type `{}`; available:\n{}",
                args.agent_type,
                self.types.describe()
            )));
        };
        let model = match args.model.as_deref() {
            Some(text) => {
                if !agent_type.accepts_model_override() {
                    return Err(ToolExecutionError::invalid_args(format!(
                        "the `{}` agent type comes from a project file and runs on the session model; leave `model` out",
                        agent_type.name
                    )));
                }
                ModelSpec::parse(text).map_err(ToolExecutionError::invalid_args)?
            }
            None => agent_type.model.clone().unwrap_or(ModelSpec::Main),
        };

        let limits = *self.tree.limits();
        let mut tools = agent_type.toolset(&self.tree.tools_of(&caller));
        let depth = self.tree.depth_of(&caller) + 1;
        if depth >= limits.max_depth {
            tools.retain(|tool| tool != "agent");
        }
        let mode = own_mode(agent_type.mode, self.tree.parent_posture(&caller));
        let background = args.background.unwrap_or(false);
        let id = self
            .tree
            .reserve(
                &caller,
                &agent_type.name,
                &model.as_text(),
                tools.clone(),
                mode,
                background,
            )
            .map_err(|refusal| match refusal {
                Refusal::Parallel(max) => ToolExecutionError::refused(format!(
                    "concurrent subagent limit reached (max_parallel = {max}). Do not retry this call: wait for a running subagent to finish, or do the work yourself."
                )),
                Refusal::Depth(max) => ToolExecutionError::refused(format!(
                    "subagents can nest only {max} layers below the main conversation, and you are at the limit. Do the work yourself."
                )),
                Refusal::SessionBudget(why) => ToolExecutionError::refused(format!(
                    "no more subagents can start: {why}. Do not retry this call."
                )),
            })?;

        let mut reservation = ReleaseOnDrop {
            tree: self.tree.clone(),
            id: id.clone(),
            armed: true,
        };
        let request = SpawnRequest {
            id: id.clone(),
            parent: caller.clone(),
            agent_type: agent_type.clone(),
            prompt: args.prompt,
            model,
            tools,
            mode,
            budget: limits.subagent,
            depth,
            background,
            ledger: self.ledger.clone(),
        };
        let handle: Arc<dyn ChildHandle> = match self.runner.spawn(request).await {
            Ok(handle) => handle,
            Err(why) => {
                self.tree.release(&id);
                return Err(ToolExecutionError::other(format!(
                    "the subagent could not start: {why}"
                )));
            }
        };
        self.tree.attach(&id, handle.clone());
        reservation.armed = false;
        self.ledger.append_event(AgentEvent::Spawned {
            agent: id.clone(),
            parent: caller.clone(),
            agent_type: agent_type.name.clone(),
        });
        subagent_event("spawn", &id, &agent_type.name);

        if background {
            let started = id.clone();
            let tree = self.tree.clone();
            let ledger = self.ledger.clone();
            let type_name = agent_type.name.clone();
            tokio::spawn(async move {
                let report = handle.report().await;
                // Queued before the child is marked finished, so a surface that sees no one
                // running always finds the report already waiting.
                if report.status != ChildStatus::Killed && !tree.was_killed(&id) {
                    let note = format!(
                        "Background agent {id} ({type_name}) finished: {}\n{}",
                        report.status.as_str(),
                        report_text(&report)
                    );
                    tree.notify_report(&caller, &id, note);
                }
                conclude(&tree, ledger.as_ref(), &id, &type_name, &report);
            });
            return Ok(ToolOutput::json(json!({ "agent_id": started })));
        }

        let mut guard = KillOnDrop {
            tree: self.tree.clone(),
            id: id.clone(),
            armed: true,
        };
        let report = handle.report().await;
        guard.armed = false;
        conclude(
            &self.tree,
            self.ledger.as_ref(),
            &id,
            &agent_type.name,
            &report,
        );
        match report.status {
            ChildStatus::Killed => Err(ToolExecutionError::other(format!(
                "subagent {id} was killed before it finished"
            ))),
            ChildStatus::Failed => Err(ToolExecutionError::other(format!(
                "subagent {id} failed: {}",
                report
                    .reason
                    .as_deref()
                    .unwrap_or("it ended without an answer")
            ))),
            _ => Ok(ToolOutput::text(report_text(&report))),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendMessageArgs {
    pub to: String,
    pub message: String,
}

#[derive(Clone)]
pub struct SendMessageTool {
    pub tree: Arc<AgentTree>,
    pub ledger: Arc<dyn RootLedger>,
}

impl Tool for SendMessageTool {
    const NAME: &'static str = "send_message";
    type Args = SendMessageArgs;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Send a message to another running agent: your parent, one of your subagents, a sibling \
         (another subagent of your parent) or `main`, the main conversation. The message is \
         queued and the recipient reads it at its next turn, marked with your agent id. Returns \
         `{\"status\":\"queued\"}`, or an error when the agent is not running."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "to": { "type": "string", "description": "An agent id, or `main`." },
                "message": { "type": "string", "description": "What to tell it." },
            },
            "required": ["to", "message"],
            "additionalProperties": false,
        })
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: SendMessageArgs,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let from = caller_of(context);
        if args.message.chars().count() > MESSAGE_CAP_CHARS {
            return Err(ToolExecutionError::invalid_args(format!(
                "a message is at most {MESSAGE_CAP_CHARS} characters"
            )));
        }
        match self.tree.deliver(&from, &args.to, args.message) {
            Ok(()) => {
                self.ledger
                    .append_event(AgentEvent::Message { from, to: args.to });
                Ok(ToolOutput::json(json!({ "status": "queued" })))
            }
            Err(DeliverError::NotRunning) => Err(ToolExecutionError::other("agent not running")),
            Err(DeliverError::NotReachable) => Err(ToolExecutionError::invalid_args(format!(
                "`{}` is not your parent, one of your subagents, a sibling or `main`",
                args.to
            ))),
        }
    }
}
