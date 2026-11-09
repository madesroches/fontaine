//! The user's lifecycle hooks on the runtime: where a session starts and ends, a prompt is
//! submitted, a turn stops or fails, and the conversation compacts. The hooks themselves are in
//! `crate::user_hooks`; this decides when each fires.

use super::{AgentRuntime, SessionOrigin};
use crate::config::HookEvent;
use crate::turn::TurnOutcome;
use crate::user_hooks::{LifecycleHooks, PromptGate};

impl AgentRuntime {
    /// The hooks for the events the runtime fires itself.
    pub fn lifecycle(&self) -> &LifecycleHooks {
        &self.setup.lifecycle
    }

    /// Whether the user configured a hook for `event` that the runtime fires.
    pub fn has_hook(&self, event: HookEvent) -> bool {
        self.setup.lifecycle.has(event)
    }

    /// What started the session, as a `session_start` matcher names it.
    fn start_source(&self) -> &'static str {
        match &self.origin {
            SessionOrigin::Fresh => "startup",
            SessionOrigin::Continued { how, .. } if how.starts_with("fork") || how == "branch" => {
                "fork"
            }
            SessionOrigin::Continued { .. } => "resume",
        }
    }

    pub(super) async fn fire_session_start(&self, source: &str) {
        self.setup
            .lifecycle
            .session_start(source, &self.models.main.reference.to_string())
            .await;
    }

    pub(super) async fn fire_session_start_for_origin(&self) {
        self.fire_session_start(self.start_source()).await;
    }

    /// `session_end`; `reason` is `clear`, `resume`, `prompt_input_exit` or `other`.
    pub async fn fire_session_end(&self, reason: &str) {
        self.setup.lifecycle.session_end(reason).await;
    }

    /// `user_prompt_submit` for a prompt the human sent. Context the hooks add is queued for the
    /// prompt; a block means the prompt is neither sent nor recorded.
    pub async fn prompt_submit(&self, prompt: &str) -> PromptGate {
        let gate = self.setup.lifecycle.prompt_submit(prompt).await;
        let note = match &gate {
            PromptGate::Proceed { context } => context.clone(),
            PromptGate::Blocked(_) => None,
        };
        self.state.set_prompt_context(note);
        gate
    }

    /// Drops the `user_prompt_submit` note of a gated command that ended without a turn.
    pub fn clear_prompt_context(&self) {
        self.state.clear_prompt_context();
    }

    /// The harness notes hooks added for the next root prompt, in order.
    pub fn take_hook_context(&self) -> Vec<String> {
        self.state.take_hook_context()
    }

    /// A root turn ended: a `stop` hook that blocks, or an error that fires `stop_failure`.
    /// `Some` is the note the turn continues on; the caller runs it as the next turn's prompt,
    /// before anything that follows a turn, such as a goal evaluation.
    pub async fn turn_ended(&self, outcome: &TurnOutcome) -> Option<String> {
        match outcome {
            TurnOutcome::Answer(text) => self.setup.lifecycle.stop(text).await,
            TurnOutcome::Failed(failure) => {
                self.setup
                    .lifecycle
                    .stop_failure(failure.kind, &failure.message, "", "")
                    .await;
                None
            }
            TurnOutcome::MaxTurns(_) | TurnOutcome::Stopped => None,
        }
    }

    /// `notification` for a prompt that now waits for the human; the hooks run in the
    /// background. `kind` is `permission_prompt` or `elicitation_dialog`.
    pub fn notify_hooks(&self, kind: &str, message: &str, title: &str) {
        self.setup.lifecycle.notify(kind, message, title);
    }
}
