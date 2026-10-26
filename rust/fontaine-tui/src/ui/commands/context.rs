use crate::commands;
use crate::reports;
use crate::ui::turn::{Serve, TurnEnd};
use crate::{App, Ui};
use fontaine_core::agent::AgentRuntime;
use fontaine_core::session::{CompactReason, CompactResult};

impl<R: FnMut(&mut App)> Ui<R> {
    /// `/clear`: empties the model's context and the transcript, and keeps the session.
    pub(crate) async fn clear(&mut self, runtime: &AgentRuntime, name: &str) {
        if !name.is_empty() {
            self.app.transcript.error(
                "/clear takes no name; /rename names this session, and /resume returns to a session later",
            );
            return;
        }
        match runtime.clear_conversation().await {
            Ok(()) => {
                self.app.transcript.clear();
                self.app.todos.set_items(runtime.session.todos().root());
                // They described the conversation that is gone.
                self.app.pending_notices.clear();
                self.app.transcript.notice(
                    "cleared the conversation; the session, its permissions and its mode carry on",
                );
            }
            Err(e) => self
                .app
                .transcript
                .error(fontaine_tools::error_chain::chain(&e)),
        }
    }

    /// `/context`: what fills the window.
    pub(crate) async fn context(&mut self, runtime: &AgentRuntime, argument: &str) {
        let all = match commands::context_all(argument) {
            Ok(all) => all,
            Err(e) => {
                self.app.transcript.error(e);
                return;
            }
        };
        match runtime.context_usage().await {
            Ok(usage) => self.app.transcript.notice(reports::context(&usage, all)),
            Err(e) => self
                .app
                .transcript
                .error(fontaine_tools::error_chain::chain(&e)),
        }
    }

    /// `/compact`: summarizes the conversation while still serving keys. Esc drops the call and
    /// leaves the saved conversation as it was; text typed meanwhile is queued like a turn's and
    /// handed back to the input afterwards, since no turn is there to receive it.
    pub(crate) async fn compact(
        &mut self,
        runtime: &AgentRuntime,
        focus: String,
    ) -> std::ops::ControlFlow<()> {
        use std::ops::ControlFlow::{Break, Continue};
        self.app.running = true;
        let mut closed = false;
        let result = {
            let mut run = Box::pin(runtime.session.compact_logged(
                &runtime.models.main.completion,
                runtime.models.main.catalog_billed(),
                CompactReason::Asked { focus },
                runtime.approver(),
            ));
            match self.serve_while(runtime, &mut run, Serve::Queueing).await {
                Ok(result) => Some(result),
                Err(end) => {
                    closed = matches!(end, TurnEnd::InputClosed);
                    None
                }
            }
        };
        let queued = runtime.steering.retract();
        if !queued.is_empty() {
            self.app.restore_to_input(&queued.join("\n\n"));
        }
        self.app.running = false;
        self.app.deny_pending();
        self.drain_prompts();
        match result {
            Some(CompactResult::Compacted { before, after, .. }) => {
                self.app.transcript.notice(format!(
                    "compacted the conversation: ~{} → ~{} tokens",
                    reports::tokens(before),
                    reports::tokens(after)
                ));
            }
            Some(CompactResult::Skipped) => self.app.transcript.notice("nothing to compact"),
            // Already warned through the approver.
            Some(CompactResult::Failed { .. }) => {}
            None => self.app.transcript.notice("interrupted"),
        }
        if closed { Break(()) } else { Continue(()) }
    }
}
