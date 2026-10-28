//! Messages the human types while the agent works. They wait in a queue until the root agent's
//! next tool result, which carries them to the model; only then do they enter the ledger, so the
//! guardian never judges a call by words the model has not read.

use std::sync::{Arc, Mutex};

use fontaine_tools::gate::{AgentId, ROOT_AGENT};
use fontaine_tools::lock::lock_recovering;
use rig_agent::agent::{AgentHook, HookContext, OutcomeAction, OutcomeEvent};
use rig_core::effect::Answered;
use rig_core::error::ErrorReport;
use rig_core::tool::ToolOutput;

use crate::gate::SessionState;
use crate::ledger::Record;

#[derive(Clone, Default)]
pub struct Steering {
    inner: Arc<Mutex<SteeringState>>,
}

#[derive(Default)]
struct SteeringState {
    queued: Vec<String>,
    delivered: Vec<String>,
}

impl Steering {
    fn state(&self) -> std::sync::MutexGuard<'_, SteeringState> {
        // Both buffers change only by `mem::take` and `extend`, so a panic cannot leave one half-updated.
        lock_recovering(&self.inner)
    }

    pub fn queue(&self, text: String) {
        self.state().queued.push(text);
    }

    /// The messages that have not reached the model yet, oldest first.
    pub fn queued(&self) -> Vec<String> {
        self.state().queued.clone()
    }

    /// Takes everything not yet delivered.
    pub fn retract(&self) -> Vec<String> {
        std::mem::take(&mut self.state().queued)
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
        if agent != ROOT_AGENT {
            return OutcomeAction::Proceed;
        }
        // One lock from take to delivered, so a retract cannot split the two.
        let mut state = self.steering.state();
        if state.queued.is_empty() {
            return OutcomeAction::Proceed;
        }
        let messages = std::mem::take(&mut state.queued);
        let mut text = result.output().render();
        for message in &messages {
            self.state.append(Record::UserMessage {
                text: message.clone(),
            });
            text.push_str(
                "\n\n[The user sent this message while you were working. Address it, then continue:]\n",
            );
            text.push_str(message);
        }
        state.delivered.extend(messages);
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
            agents: Arc::new(AgentTree::new(Limits::default())),
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

    #[test]
    fn queued_messages_ride_the_next_root_result_and_enter_the_ledger_in_order() {
        let (hook, steering, ledger, _dir) = hook();
        steering.queue("first".into());
        steering.queue("second".into());
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
        steering.queue("also this".into());
        let refused = answered(ToolResult::failed(ToolExecutionError::refused("denied")));
        let action = hook.steer(ROOT_AGENT, &refused);
        let result = rewritten(&action).expect("a rewritten result");
        assert!(result.is_refused());
        assert!(result.output().render().contains("also this"));

        steering.queue("and this".into());
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

        steering.queue("pending".into());
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
        steering.queue("delivered".into());
        let ok = answered(ToolResult::success(ToolOutput::text("done")));
        hook.steer(ROOT_AGENT, &ok);
        steering.queue("waiting".into());
        assert_eq!(steering.retract(), vec!["waiting"]);
        assert!(steering.retract().is_empty());
        assert_eq!(steering.take_delivered(), vec!["delivered"]);
    }
}
