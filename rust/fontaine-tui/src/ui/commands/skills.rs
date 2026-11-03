use std::ops::ControlFlow;

use fontaine_core::agent::AgentRuntime;
use fontaine_core::extensions::Scope;
use fontaine_core::skill_trust::{self, TrustState};

use crate::skill_trust_prompt::SkillTrustPrompt;
use crate::{Action, Ui};

impl<R: FnMut(&mut crate::App)> Ui<R> {
    /// `/skills trust <name>`: after a confirmation that shows what the file declares, records the
    /// user's trust in it. Interactive-only. The running session's grants do not change: the
    /// record is read at the next session start.
    pub(crate) async fn skills_command(
        &mut self,
        runtime: &mut AgentRuntime,
        argument: &str,
    ) -> ControlFlow<()> {
        let mut words = argument.split_whitespace();
        let (Some("trust"), Some(name), None) = (words.next(), words.next(), words.next()) else {
            self.app
                .transcript
                .error("usage: /skills, or /skills trust <name>");
            return ControlFlow::Continue(());
        };
        let extensions = runtime.extensions().clone();
        let Some(skill) = extensions.skill(name) else {
            self.app
                .transcript
                .error(format!("no skill named `{name}`; /skills lists them"));
            return ControlFlow::Continue(());
        };
        if skill.scope == Scope::Project {
            self.app.transcript.error(format!(
                "`{name}` is a project skill: a repository's skill is never trusted"
            ));
            return ControlFlow::Continue(());
        }
        if extensions.trust_state(skill) == TrustState::Trusted {
            self.app
                .transcript
                .notice(format!("`{name}` is already trusted at this version"));
            return ControlFlow::Continue(());
        }
        self.app.begin_skill_trust(SkillTrustPrompt::new(
            skill,
            extensions.trusted.record(name),
        ));
        loop {
            self.draw(runtime);
            let Some(input) = self.input.recv().await else {
                self.app.skill_trust_prompt = None;
                return ControlFlow::Continue(());
            };
            match self.app.handle_input(input) {
                Action::SkillTrust(true) => {
                    match skill_trust::trust(
                        runtime.config_dir(),
                        vec![skill_trust::record_of(skill)],
                    ) {
                        Ok(()) => {
                            runtime.state.record_skill_trust(&skill.name, &skill.digest);
                            self.app.transcript.notice(format!(
                                "`{name}` trusted: its allowed-tools count from the next session start"
                            ));
                        }
                        Err(e) => self.app.transcript.error(format!(
                            "could not record the trust: {}",
                            fontaine_tools::error_chain::chain(&e)
                        )),
                    }
                    return ControlFlow::Continue(());
                }
                Action::SkillTrust(false) => {
                    self.app.transcript.notice("trust not recorded");
                    return ControlFlow::Continue(());
                }
                Action::Quit => return ControlFlow::Break(()),
                _ => {}
            }
        }
    }
}
