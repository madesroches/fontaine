//! Messages the human types while the agent works. They wait in a queue until the root agent's
//! next tool result, which carries them to the model; only then do they enter the ledger, so the
//! guardian never judges a call by words the model has not read.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fontaine_tools::gate::{AgentId, ROOT_AGENT};
use fontaine_tools::lock::lock_recovering;
use rig_agent::agent::{AgentHook, HookContext, OutcomeAction, OutcomeEvent};
use rig_core::effect::Answered;
use rig_core::error::ErrorReport;
use rig_core::tool::ToolOutput;

use crate::gate::SessionState;
use crate::ledger::Record;

/// One queued input: a plain message, a skill invocation as typed, or a built-in command
/// or `!` shell line as typed. Only plain messages can be steered into a running turn;
/// the others wait for it to end and keep everything behind them in line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Queued {
    Message(String),
    Skill {
        raw: String,
        name: String,
        argument: String,
    },
    /// A built-in command exactly as typed (`/compact fix tests`); re-parsed and dispatched
    /// when it executes.
    Command(String),
    /// A `!` shell command as typed, prefix included (`!cargo test`); executed as a shell
    /// command when the turn ends.
    Shell(String),
}

impl Queued {
    /// The text as typed: the message, the slash command line or the `!` line. Used for the
    /// queue display, the input restore and the history.
    pub fn raw(&self) -> &str {
        match self {
            Queued::Message(text) => text,
            Queued::Skill { raw, .. } => raw,
            Queued::Command(raw) => raw,
            Queued::Shell(raw) => raw,
        }
    }
}

#[derive(Clone, Default)]
pub struct Steering {
    inner: Arc<Mutex<SteeringState>>,
}

#[derive(Default)]
struct SteeringState {
    queued: Vec<Queued>,
    delivered: Vec<String>,
    /// While held, nothing is delivered: replay keeps typed order across queued entries
    /// that already left the queue.
    hold: bool,
    /// What the human typed in a subagent's view, by agent.
    agents: HashMap<String, AgentQueue>,
}

#[derive(Default)]
struct AgentQueue {
    queued: Vec<String>,
    delivered: Vec<String>,
}

impl Steering {
    fn state(&self) -> std::sync::MutexGuard<'_, SteeringState> {
        // Both buffers change only by `mem::take` and `extend`, so a panic cannot leave one half-updated.
        lock_recovering(&self.inner)
    }

    pub fn queue(&self, entry: Queued) {
        self.state().queued.push(entry);
    }

    /// The entries that have not reached the model yet, as typed, oldest first.
    pub fn queued(&self) -> Vec<String> {
        self.state()
            .queued
            .iter()
            .map(|entry| entry.raw().to_string())
            .collect()
    }

    /// Takes everything not yet delivered, as typed.
    pub fn retract(&self) -> Vec<Queued> {
        std::mem::take(&mut self.state().queued)
    }

    /// Holds or releases replay delivery: while held, the hook leaves the whole queue
    /// untouched and appends no ledger records. Display and retract behave as usual.
    pub fn set_delivery_hold(&self, held: bool) {
        self.state().hold = held;
    }

    /// Whether `steer` would deliver now: a plain message leads the queue and delivery is not
    /// held.
    pub fn has_deliverable_message(&self) -> bool {
        let state = self.state();
        !state.hold && matches!(state.queued.first(), Some(Queued::Message(_)))
    }

    /// Queues what the human typed in `agent`'s view. The agent reads it with its next tool
    /// result.
    pub fn queue_for(&self, agent: &str, text: String) {
        self.state()
            .agents
            .entry(agent.to_string())
            .or_default()
            .queued
            .push(text);
    }

    /// What the human typed to `agent` that it has not read yet, oldest first.
    pub fn queued_for(&self, agent: &str) -> Vec<String> {
        self.state()
            .agents
            .get(agent)
            .map(|queue| queue.queued.clone())
            .unwrap_or_default()
    }

    /// What `agent` has read since the last call, oldest first.
    pub fn take_delivered_for(&self, agent: &str) -> Vec<String> {
        self.state()
            .agents
            .get_mut(agent)
            .map(|queue| std::mem::take(&mut queue.delivered))
            .unwrap_or_default()
    }

    /// Takes what the human typed to `agent` that it never read, such as when its run ended
    /// without another tool call.
    pub fn take_pending_for(&self, agent: &str) -> Vec<String> {
        self.state()
            .agents
            .get_mut(agent)
            .map(|queue| std::mem::take(&mut queue.queued))
            .unwrap_or_default()
    }

    /// Whether `agent`'s next tool result would carry a message: the wake-up for an agent parked
    /// in a long wait.
    pub fn has_deliverable_message_for(&self, agent: &str) -> bool {
        if agent == ROOT_AGENT {
            return self.has_deliverable_message();
        }
        self.state()
            .agents
            .get(agent)
            .is_some_and(|queue| !queue.queued.is_empty())
    }

    /// The messages delivered since the last call, oldest first.
    pub fn take_delivered(&self) -> Vec<String> {
        std::mem::take(&mut self.state().delivered)
    }
}

/// Appends the queued messages to the root agent's next tool result.
pub struct SteeringHook {
    steering: Steering,
    state: Arc<SessionState>,
}

impl SteeringHook {
    pub fn new(steering: Steering, state: Arc<SessionState>) -> SteeringHook {
        SteeringHook { steering, state }
    }

    fn steer(&self, agent: &str, outcome: &Result<Answered, ErrorReport>) -> OutcomeAction {
        let Ok(Answered::ToolResult { result }) = outcome else {
            return OutcomeAction::Proceed;
        };
        // One lock from take to delivered, so a retract cannot split the two.
        let mut state = self.steering.state();
        let messages: Vec<String> = if agent == ROOT_AGENT {
            if state.queued.is_empty() || state.hold {
                return OutcomeAction::Proceed;
            }
            // Only the leading run of plain messages can ride a tool result; a skill, command
            // or shell line needs the turn to end, and nothing queued behind one may overtake
            // it.
            let leading = state
                .queued
                .iter()
                .take_while(|entry| matches!(entry, Queued::Message(_)))
                .count();
            if leading == 0 {
                return OutcomeAction::Proceed;
            }
            state
                .queued
                .drain(..leading)
                .map(|entry| match entry {
                    Queued::Message(text) => text,
                    // The take above counts message entries only.
                    _ => unreachable!("only message entries are drained"),
                })
                .collect()
        } else {
            let Some(queue) = state.agents.get_mut(agent) else {
                return OutcomeAction::Proceed;
            };
            if queue.queued.is_empty() {
                return OutcomeAction::Proceed;
            }
            std::mem::take(&mut queue.queued)
        };
        let target = (agent != ROOT_AGENT).then(|| agent.to_string());
        let mut text = result.output().render();
        for message in &messages {
            self.state.append(Record::UserMessage {
                text: message.clone(),
                agent: target.clone(),
            });
            text.push_str(
                "\n\n[The user sent this message while you were working. Address it, then continue:]\n",
            );
            text.push_str(message);
        }
        match &target {
            None => state.delivered.extend(messages),
            Some(agent) => state
                .agents
                .entry(agent.to_string())
                .or_default()
                .delivered
                .extend(messages),
        }
        OutcomeAction::Replace(Ok(Answered::ToolResult {
            result: result.clone().with_output(ToolOutput::text(text)),
        }))
    }
}

impl AgentHook for SteeringHook {
    async fn on_outcome(&self, _ctx: &HookContext, event: OutcomeEvent<'_>) -> OutcomeAction {
        let agent = event
            .tool_context()
            .and_then(|context| context.get::<AgentId>().ok().flatten())
            .map_or_else(|| ROOT_AGENT.to_string(), |id| id.0);
        self.steer(&agent, event.outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::AuditLog;
    use crate::config::PermissionMode;
    use crate::gate::{AutoContext, StateParams, no_guardian};
    use crate::ledger::Ledger;
    use crate::permissions::DenyAll;
    use fontaine_agents::tree::{AgentTree, Limits};
    use fontaine_tools::scope::Scope;
    use rig_core::tool::{ToolExecutionError, ToolResult};

    fn hook() -> (SteeringHook, Steering, Arc<Ledger>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(Ledger::create(dir.path()).unwrap());
        let agents = Arc::new(AgentTree::new(Limits::default(), 1));
        agents.set_root_tools(|| {
            fontaine_tools::descriptor::names()
                .map(String::from)
                .collect()
        });
        let state = Arc::new(SessionState::new(StateParams {
            session_id: "steering-test".into(),
            mode: PermissionMode::Manual,
            scope: Arc::new(Scope::new(dir.path().to_path_buf(), [])),
            deny: Vec::new(),
            ask: Vec::new(),
            allow: Vec::new(),
            ledger: ledger.clone(),
            audit: Arc::new(AuditLog::create(dir.path(), "steering-test").unwrap()),
            approver: Arc::new(DenyAll),
            home: None,
            protected_dirs: Vec::new(),
            scratch: None,
            memory_root: None,
            guardian: no_guardian(),
            auto: AutoContext::default(),
            agents: agents.clone(),
            agent_types: Arc::new(fontaine_agents::registry::AgentTypes::builtin()),
            reveal_mcp_names: false,
            auto_edits: crate::gate::AutoEdits::default(),
        }));
        let steering = Steering::default();
        (
            SteeringHook::new(steering.clone(), state),
            steering,
            ledger,
            dir,
        )
    }

    #[allow(clippy::result_large_err)]
    fn answered(result: ToolResult) -> Result<Answered, ErrorReport> {
        Ok(Answered::ToolResult { result })
    }

    fn rewritten(action: &OutcomeAction) -> Option<&ToolResult> {
        match action {
            OutcomeAction::Replace(Ok(Answered::ToolResult { result })) => Some(result),
            _ => None,
        }
    }

    fn skill(raw: &str) -> Queued {
        let name = raw
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap();
        Queued::Skill {
            raw: raw.into(),
            name: name.into(),
            argument: raw.trim_start_matches('/')[name.len()..].trim().into(),
        }
    }

    #[test]
    fn only_a_leading_unheld_message_is_deliverable() {
        let steering = Steering::default();
        assert!(!steering.has_deliverable_message());
        steering.queue(skill("/review"));
        steering.queue(Queued::Message("later".into()));
        assert!(!steering.has_deliverable_message());
        steering.retract();
        steering.queue(Queued::Command("/compact".into()));
        assert!(!steering.has_deliverable_message());
        steering.retract();
        steering.queue(Queued::Message("now".into()));
        assert!(steering.has_deliverable_message());
        steering.set_delivery_hold(true);
        assert!(!steering.has_deliverable_message());
    }

    #[test]
    fn queued_messages_ride_the_next_root_result_and_enter_the_ledger_in_order() {
        let (hook, steering, ledger, _dir) = hook();
        steering.queue(Queued::Message("first".into()));
        steering.queue(Queued::Message("second".into()));
        let outcome = answered(ToolResult::success(ToolOutput::text("done")));
        let action = hook.steer(ROOT_AGENT, &outcome);
        let result = rewritten(&action).expect("a rewritten result");
        let text = result.output().render();
        assert!(
            text.starts_with("done\n\n[The user sent this message"),
            "{text}"
        );
        assert!(text.find("first").unwrap() < text.find("second").unwrap());
        assert!(result.is_success());
        assert_eq!(ledger.view().user_messages, vec!["first", "second"]);
        assert_eq!(steering.take_delivered(), vec!["first", "second"]);
        assert!(steering.queued().is_empty());
        assert!(steering.take_delivered().is_empty());
    }

    #[test]
    fn a_refused_or_failed_result_carries_the_messages_and_keeps_its_status() {
        let (hook, steering, _ledger, _dir) = hook();
        steering.queue(Queued::Message("also this".into()));
        let refused = answered(ToolResult::failed(ToolExecutionError::refused("denied")));
        let action = hook.steer(ROOT_AGENT, &refused);
        let result = rewritten(&action).expect("a rewritten result");
        assert!(result.is_refused());
        assert!(result.output().render().contains("also this"));

        steering.queue(Queued::Message("and this".into()));
        let failed = answered(ToolResult::failed(ToolExecutionError::other("boom")));
        let action = hook.steer(ROOT_AGENT, &failed);
        let result = rewritten(&action).expect("a rewritten result");
        assert!(result.is_error());
        assert!(result.output().render().contains("and this"));
    }

    #[test]
    fn results_pass_untouched_without_messages_for_errors_and_for_other_agents() {
        let (hook, steering, ledger, _dir) = hook();
        let ok = answered(ToolResult::success(ToolOutput::text("done")));
        assert!(matches!(
            hook.steer(ROOT_AGENT, &ok),
            OutcomeAction::Proceed
        ));

        steering.queue(Queued::Message("pending".into()));
        let error: Result<Answered, ErrorReport> =
            Err(ErrorReport::new(rig_core::ErrorKind::Other, "model failed"));
        assert!(matches!(
            hook.steer(ROOT_AGENT, &error),
            OutcomeAction::Proceed
        ));
        assert!(matches!(hook.steer("sub-1", &ok), OutcomeAction::Proceed));
        assert_eq!(steering.queued(), vec!["pending"]);
        assert!(ledger.view().user_messages.is_empty());
    }

    #[test]
    fn retract_returns_only_what_was_not_delivered() {
        let (hook, steering, _ledger, _dir) = hook();
        steering.queue(Queued::Message("delivered".into()));
        let ok = answered(ToolResult::success(ToolOutput::text("done")));
        hook.steer(ROOT_AGENT, &ok);
        steering.queue(Queued::Message("waiting".into()));
        assert_eq!(steering.retract(), vec![Queued::Message("waiting".into())]);
        assert!(steering.retract().is_empty());
        assert_eq!(steering.take_delivered(), vec!["delivered"]);
    }

    #[test]
    fn a_skill_or_command_entry_blocks_delivery_of_itself_and_everything_behind_it() {
        let (hook1, steering, ledger, _dir) = hook();
        steering.queue(Queued::Message("before".into()));
        steering.queue(skill("/review 42"));
        steering.queue(Queued::Message("behind".into()));
        steering.queue(Queued::Command("/rename later".into()));
        let ok = answered(ToolResult::success(ToolOutput::text("done")));
        let action = hook1.steer(ROOT_AGENT, &ok);
        let result = rewritten(&action).expect("a rewritten result");
        let text = result.output().render();
        assert!(text.contains("before"), "{text}");
        assert!(!text.contains("/review 42"), "{text}");
        assert!(!text.contains("behind"), "{text}");
        assert_eq!(ledger.view().user_messages, vec!["before"]);
        // The barrier and everything behind it stay queued, typed and in order.
        assert_eq!(
            steering.retract(),
            vec![
                skill("/review 42"),
                Queued::Message("behind".into()),
                Queued::Command("/rename later".into()),
            ]
        );

        // A queue that starts with a barrier delivers nothing at all.
        let (hook2, steering, ledger, _dir) = hook();
        steering.queue(Queued::Shell("!echo hi".into()));
        steering.queue(Queued::Message("behind".into()));
        let action = hook2.steer(ROOT_AGENT, &ok);
        assert!(matches!(action, OutcomeAction::Proceed));
        assert!(ledger.view().user_messages.is_empty());
        assert_eq!(
            steering.retract(),
            vec![
                Queued::Shell("!echo hi".into()),
                Queued::Message("behind".into()),
            ]
        );
    }

    #[test]
    fn a_delivery_hold_leaves_the_queue_and_the_ledger_untouched_until_released() {
        let (hook, steering, ledger, _dir) = hook();
        steering.queue(Queued::Message("held".into()));
        steering.set_delivery_hold(true);
        let ok = answered(ToolResult::success(ToolOutput::text("done")));
        assert!(matches!(
            hook.steer(ROOT_AGENT, &ok),
            OutcomeAction::Proceed
        ));
        assert!(ledger.view().user_messages.is_empty());
        assert_eq!(steering.queued(), vec!["held"]);
        // Releasing the hold restores ordinary delivery.
        steering.set_delivery_hold(false);
        let action = hook.steer(ROOT_AGENT, &ok);
        let result = rewritten(&action).expect("a rewritten result");
        assert!(result.output().render().contains("held"));
        assert_eq!(ledger.view().user_messages, vec!["held"]);
        assert!(steering.queued().is_empty());
    }

    #[test]
    fn queued_returns_raws_and_retract_returns_typed_entries() {
        let (_hook, steering, _ledger, _dir) = hook();
        steering.queue(Queued::Message("plain".into()));
        steering.queue(skill("/review 42"));
        steering.queue(Queued::Command("/rename later".into()));
        steering.queue(Queued::Shell("!echo hi".into()));
        assert_eq!(
            steering.queued(),
            vec!["plain", "/review 42", "/rename later", "!echo hi"]
        );
        let retracted = steering.retract();
        assert_eq!(retracted[1], skill("/review 42"));
        assert_eq!(retracted[3], Queued::Shell("!echo hi".into()));
        assert!(steering.retract().is_empty());
    }

    #[test]
    fn a_message_for_a_child_rides_its_next_result_and_enters_the_ledger_for_that_agent() {
        let (hook, steering, ledger, _dir) = hook();
        steering.queue(Queued::Message("for main".into()));
        steering.queue_for("agent-1", "count the lines".into());
        assert_eq!(steering.queued_for("agent-1"), vec!["count the lines"]);
        let ok = answered(ToolResult::success(ToolOutput::text("done")));

        let action = hook.steer("agent-1", &ok);
        let text = rewritten(&action)
            .expect("a rewritten result")
            .output()
            .render();
        assert!(text.contains("The user sent this message while you were working"));
        assert!(text.contains("count the lines"));
        assert!(!text.contains("for main"));

        // Only then does the ledger hold it, and for that agent alone.
        assert!(ledger.view().user_messages.is_empty());
        assert_eq!(ledger.view().agent_messages.len(), 1);
        assert_eq!(ledger.view().agent_messages[0].agent, "agent-1");
        assert_eq!(
            steering.take_delivered_for("agent-1"),
            vec!["count the lines"]
        );
        assert!(steering.take_delivered_for("agent-1").is_empty());

        // The root's queue is untouched.
        assert_eq!(steering.queued(), vec!["for main"]);
        assert!(steering.take_delivered().is_empty());
    }

    #[test]
    fn a_childs_queue_never_steers_the_root_or_another_child() {
        let (hook, steering, ledger, _dir) = hook();
        steering.queue_for("agent-1", "only for one".into());
        let ok = answered(ToolResult::success(ToolOutput::text("done")));
        assert!(matches!(
            hook.steer(ROOT_AGENT, &ok),
            OutcomeAction::Proceed
        ));
        assert!(matches!(hook.steer("agent-2", &ok), OutcomeAction::Proceed));
        assert_eq!(steering.queued_for("agent-1"), vec!["only for one"]);
        assert!(ledger.view().agent_messages.is_empty());
        assert!(ledger.view().user_messages.is_empty());
    }

    #[test]
    fn a_child_parked_in_a_wait_is_woken_by_its_own_message_only() {
        let steering = Steering::default();
        assert!(!steering.has_deliverable_message_for("agent-1"));
        steering.queue_for("agent-1", "wake up".into());
        assert!(steering.has_deliverable_message_for("agent-1"));
        assert!(!steering.has_deliverable_message_for("agent-2"));
        assert!(!steering.has_deliverable_message_for(ROOT_AGENT));
        steering.queue(Queued::Message("main".into()));
        assert!(steering.has_deliverable_message_for(ROOT_AGENT));
    }

    #[test]
    fn what_a_child_never_read_can_be_taken_back_once() {
        let steering = Steering::default();
        steering.queue_for("agent-1", "one".into());
        steering.queue_for("agent-1", "two".into());
        assert_eq!(steering.take_pending_for("agent-1"), vec!["one", "two"]);
        assert!(steering.take_pending_for("agent-1").is_empty());
        assert!(steering.queued_for("agent-1").is_empty());
    }
}
