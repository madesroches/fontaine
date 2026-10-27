use std::ops::ControlFlow::{self, Continue};

use crate::rewind::RewindPicker;
use crate::{App, Ui};
use fontaine_core::agent::AgentRuntime;
use fontaine_core::checkpoint::{RewindPlan, RewindWhat};
use fontaine_tools::error_chain::chain;

impl<R: FnMut(&mut App)> Ui<R> {
    /// `/rewind` and Esc Esc: opens the picker over the prompts of this conversation.
    pub(crate) fn rewind(&mut self, runtime: &AgentRuntime, argument: &str) -> ControlFlow<()> {
        if !argument.is_empty() {
            self.app.transcript.error(format!(
                "/rewind takes no argument, not `{argument}`; it opens a list of your prompts"
            ));
            return Continue(());
        }
        self.open_rewind(runtime);
        Continue(())
    }

    pub(crate) fn open_rewind(&mut self, runtime: &AgentRuntime) {
        let picker = RewindPicker::new(runtime.rewind_turns());
        if picker.is_empty() {
            self.app
                .transcript
                .notice("no prompt to rewind to yet in this conversation");
            return;
        }
        self.app.rewind = Some(picker);
    }

    /// The human picked a prompt: the picker shows what rewinding to it would do.
    pub(crate) fn offer_rewind(&mut self, runtime: &AgentRuntime, turn: u64) {
        match runtime.rewind_plan(turn) {
            Some(plan) => {
                if let Some(picker) = &mut self.app.rewind {
                    picker.offer(plan);
                }
            }
            None => {
                self.app.rewind = None;
                self.app
                    .transcript
                    .error("that prompt can no longer be rewound to");
            }
        }
    }

    pub(crate) async fn run_rewind(
        &mut self,
        runtime: &AgentRuntime,
        plan: RewindPlan,
        what: RewindWhat,
        overwrite_changed: bool,
    ) {
        let had_goal = runtime.session.goal().is_active();
        let outcome = match runtime.rewind(&plan, what, overwrite_changed).await {
            Ok(outcome) => outcome,
            Err(e) => {
                self.app.transcript.error(chain(&e));
                return;
            }
        };
        let had_goal = had_goal && outcome.conversation_restored;
        let mut lines = Vec::new();
        if outcome.conversation_restored {
            self.app.transcript.clear();
            self.app.todos.set_items(runtime.session.todos().root());
            // They described the conversation that is gone.
            self.app.pending_notices.clear();
            lines.push(
                "rewound the conversation to before that prompt; the session, its permissions and its mode carry on"
                    .to_string(),
            );
        }
        if let Some(code) = &outcome.code {
            let count =
                |n: usize, what: &str| format!("{n} file{} {what}", if n == 1 { "" } else { "s" });
            lines.push(format!(
                "code: {}, {}",
                count(code.restored.len(), "restored"),
                count(code.deleted.len(), "deleted")
            ));
            for (title, paths) in [
                ("restored", &code.restored),
                ("deleted", &code.deleted),
                ("kept (changed since)", &code.kept_changed),
            ] {
                for path in paths {
                    lines.push(format!("  {title}: {}", path.display()));
                }
            }
            for (path, why) in &code.not_restorable {
                lines.push(format!("  not restorable: {} ({why})", path.display()));
            }
            for (path, e) in &code.failed {
                lines.push(format!("  failed: {}: {}", path.display(), chain(e)));
            }
            lines.push(
                "changes made through the shell, `!` commands or MCP tools were not touched"
                    .to_string(),
            );
        }
        if had_goal {
            lines.push("the goal was cleared with it".to_string());
        }
        self.app.transcript.notice(lines.join("\n"));
        if outcome.conversation_restored {
            self.app.restore_to_input(&outcome.prompt);
        }
    }
}
