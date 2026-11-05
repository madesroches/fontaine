use std::ops::ControlFlow;

use crate::{Action, Ui};
use fontaine_core::agent::AgentRuntime;

impl<R: FnMut(&mut crate::App)> Ui<R> {
    /// `/guardian-disable`: after a confirmation, turns the guardian and the permission floor
    /// off for the rest of this session. Interactive-only; nothing is persisted and subagents
    /// are never loosened by it.
    pub(crate) async fn guardian_disable(&mut self, runtime: &mut AgentRuntime) -> ControlFlow<()> {
        if runtime.state.guardian_off() {
            self.app
                .transcript
                .notice("the guardian is already off for this session");
            return ControlFlow::Continue(());
        }
        let afk = runtime.state.afk();
        if afk.stopped.is_some() || afk.stretch.is_some() {
            let message = if afk.stopped.is_some() {
                fontaine_core::gate::AfkRefusal::Stopped.to_string()
            } else {
                let refusal = fontaine_core::gate::AfkRefusal::AfkOn.to_string();
                match crate::statusbar::cycle_keys(&self.app) {
                    Some(key) => format!("{refusal} ({key})"),
                    None => refusal,
                }
            };
            self.app.transcript.error(message);
            return ControlFlow::Continue(());
        }
        self.app.begin_guardian_off();
        loop {
            self.draw(runtime);
            let Some(input) = self.input.recv().await else {
                self.app.guardian_off_prompt = None;
                return ControlFlow::Continue(());
            };
            let action = self.app.handle_input(input);
            match action {
                Action::GuardianOff(true) => {
                    match runtime.state.set_guardian_off(true) {
                        Ok(()) => self.app.transcript.notice(
                            "guardian off for this session: every call runs without the guardian or the permission rules — /guardian-enable turns it back on",
                        ),
                        Err(refusal) => self.app.transcript.error(refusal.to_string()),
                    }
                    return ControlFlow::Continue(());
                }
                Action::GuardianOff(false) => {
                    self.app.transcript.notice("guardian left on");
                    return ControlFlow::Continue(());
                }
                Action::Quit => return ControlFlow::Break(()),
                _ => {}
            }
        }
    }

    /// `/guardian-enable`: turns the guardian and the permission floor back on at once.
    pub(crate) fn guardian_enable(&mut self, runtime: &AgentRuntime) {
        if !runtime.state.guardian_off() {
            self.app
                .transcript
                .notice("the guardian is already on for this session");
            return;
        }
        match runtime.state.set_guardian_off(false) {
            Ok(()) => self.app.transcript.notice("guardian on"),
            Err(refusal) => self.app.transcript.error(refusal.to_string()),
        }
    }
}
