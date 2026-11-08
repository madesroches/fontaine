use crate::model_picker::{Group, ModelPicker, PickerPurpose};
use crate::ui::commands::config::overriding_scope;
use crate::{App, Ui, UnavailableAnswer};
use fontaine_core::agent::AgentRuntime;
use fontaine_core::config::Slot;
use fontaine_core::effort::Effort;
use fontaine_core::models::GuardianModel;
use fontaine_core::permissions::UnavailableChoice;

impl<R: FnMut(&mut App)> Ui<R> {
    /// `/model`: opens the picker, or switches the live session to the model named and saves
    /// the choice.
    pub(crate) async fn model(&mut self, runtime: &mut AgentRuntime, reference: &str) {
        if reference.is_empty() {
            self.open_model_picker(runtime, None).await;
            return;
        }
        match runtime.switch_main_model(reference).await {
            Ok(switched) => {
                let mut message = format!("model is now {}", switched.reference);
                if switched.guardian_followed {
                    message.push_str("; the guardian runs on it too");
                }
                self.remember_model(PickerPurpose::Main, &switched.reference);
                self.finish_switch(
                    message,
                    switched.persist_error,
                    overriding_scope(&runtime.config, "model.model"),
                );
                if let Some(notice) = runtime.models.main.facts_notice.clone() {
                    self.app.transcript.warning(notice, None);
                }
            }
            Err(e) => self.app.transcript.error(format!(
                "/model: {}",
                fontaine_tools::error_chain::chain(&e)
            )),
        }
    }

    /// `/effort`: shows the session's reasoning effort and the levels the model takes, or sets
    /// it for this session. It is not saved: `[model] effort` is the standing choice.
    pub(crate) fn effort(&mut self, runtime: &AgentRuntime, text: &str) {
        let supported = takes(&runtime.models.main.supported_efforts());
        if text.is_empty() {
            let current = runtime.state.effort().map_or_else(
                || "unset (the model's own default)".to_string(),
                |e| e.to_string(),
            );
            self.app.transcript.notice(format!(
                "effort: {current} · {} takes {supported} · /effort <level> sets it for this session, /effort unset clears it",
                runtime.models.main.reference
            ));
            return;
        }
        let level = if text == "unset" {
            None
        } else {
            match text.parse::<Effort>() {
                Ok(level) => Some(level),
                Err(e) => {
                    self.app.transcript.error(format!("/effort: {e}, or unset"));
                    return;
                }
            }
        };
        runtime.state.set_effort(level);
        self.app.effort = level;
        // A level the model cannot take is warned about here rather than at the next turn.
        runtime.turn_generation_options();
        self.app.transcript.notice(match level {
            Some(level) => format!("effort is now {level} for this session"),
            None => "effort is unset for this session: the model's own default applies".to_string(),
        });
    }

    /// Lists the models of every account, or of `account` alone, which can take a while, then
    /// opens the picker.
    pub(crate) async fn open_model_picker(
        &mut self,
        runtime: &mut AgentRuntime,
        account: Option<&str>,
    ) {
        let Some(groups) = self.list_groups(runtime, account, "/model").await else {
            return;
        };
        let current = runtime.models.main.reference.clone();
        let scope = account.map_or(String::new(), |name| format!("models of {name} · "));
        self.app.models = Some(ModelPicker::new(
            PickerPurpose::Main,
            format!(
                "{scope}model {current} · guardian {}",
                runtime.state.guardian().label()
            ),
            Some(current),
            self.recent.list(PickerPurpose::Main),
            groups,
        ));
    }

    /// `/guardian-model`: opens the picker over a live listing, or pins the
    /// guardian to the one named.
    pub(crate) async fn guardian_model(&mut self, runtime: &mut AgentRuntime, reference: &str) {
        if reference.is_empty() {
            self.open_guardian_picker(runtime).await;
            return;
        }
        match runtime.set_guardian_model(reference).await {
            Ok(switched) => {
                let guardian = runtime.state.guardian();
                match guardian.unavailable_reason() {
                    Some(reason) => self.app.transcript.error(format!(
                        "guardian is now {}, but it is unavailable: {reason}",
                        switched.reference
                    )),
                    None => {
                        self.remember_model(PickerPurpose::Guardian, &switched.reference);
                        self.finish_switch(
                            format!("guardian is now {}", switched.reference),
                            switched.persist_error,
                            overriding_scope(&runtime.config, "guardian.model"),
                        );
                        let notice = match &runtime.models.guardian {
                            Some(GuardianModel::Ready(handle)) => handle.facts_notice.clone(),
                            _ => None,
                        };
                        if let Some(notice) = notice {
                            self.app.transcript.warning(notice, None);
                        }
                    }
                }
            }
            Err(e) => self.app.transcript.error(format!(
                "/guardian-model: {}",
                fontaine_tools::error_chain::chain(&e)
            )),
        }
    }

    async fn list_groups(
        &mut self,
        runtime: &mut AgentRuntime,
        account: Option<&str>,
        command: &str,
    ) -> Option<Vec<Group>> {
        self.app.transcript.notice("listing models…");
        self.draw(runtime);
        match runtime.model_listings(account).await {
            Ok(listings) => Some(listings.iter().map(Group::from).collect()),
            Err(e) => {
                self.app.transcript.error(format!(
                    "{command}: {}",
                    fontaine_tools::error_chain::chain(&e)
                ));
                None
            }
        }
    }

    /// Lists every account's models live, like `/model`: a pick the account cannot serve is
    /// still installed and reported as an unavailable guardian, so filtering here would only
    /// hide accounts silently.
    pub(crate) async fn open_guardian_picker(&mut self, runtime: &mut AgentRuntime) {
        let Some(groups) = self.list_groups(runtime, None, "/guardian-model").await else {
            return;
        };
        let guardian = runtime.state.guardian();
        let mut header = format!("guardian {}", guardian.label());
        if let Some(reason) = guardian.unavailable_reason() {
            header.push_str(&format!(" (unavailable: {reason})"));
        }
        // The runtime's own record of the guardian goes stale after a session-only change.
        let current = fontaine_core::models::ModelRef::parse(guardian.label());
        self.app.models = Some(ModelPicker::new(
            PickerPurpose::Guardian,
            header,
            current,
            self.recent.list(PickerPurpose::Guardian),
            groups,
        ));
    }

    /// A pick from the picker is the argument form of the command.
    pub(crate) async fn apply_pick(
        &mut self,
        runtime: &mut AgentRuntime,
        purpose: PickerPurpose,
        reference: &str,
    ) {
        match purpose {
            PickerPurpose::Main => self.model(runtime, reference).await,
            PickerPurpose::Guardian => self.guardian_model(runtime, reference).await,
        }
    }

    /// Adds a model the human switched to into the picker's Recent group; a file that cannot be
    /// written is reported once.
    fn remember_model(&mut self, purpose: PickerPurpose, reference: &str) {
        let Some(model) = fontaine_core::models::ModelRef::parse(reference) else {
            return;
        };
        if let Some(error) = self.recent.record(purpose, &model) {
            self.app.transcript.notice(error);
        }
    }

    /// `overridden_by` is the scope outranking the user file for the key just saved, if any.
    pub(crate) fn finish_switch(
        &mut self,
        message: String,
        persist_error: Option<String>,
        overridden_by: Option<Slot>,
    ) {
        match persist_error {
            None => {
                let note = overridden_by.map_or(String::new(), |scope| {
                    format!(
                        "; {} sets this key and wins from the next launch",
                        scope.label()
                    )
                });
                self.app
                    .transcript
                    .notice(format!("{message} (saved to your settings{note})"));
            }
            Some(why) => self
                .app
                .transcript
                .notice(format!("{message} for this session only: {why}")),
        }
    }

    /// `r` in `/denied`: a human approval of that call, once, and a note to the agent.
    pub(crate) fn retry_denied(&mut self, runtime: &AgentRuntime, index: usize) {
        let Some(call) = runtime.state.approve_retry(index) else {
            return;
        };
        if let Some(list) = &mut self.app.denied {
            list.mark_retried(index);
        }
        self.app.pending_notices.push(format!(
            "[Harness note, not the user's words: the user reviewed your denied `{}` call ({}) and approved it once. You may retry exactly that call.]",
            call.tool, call.summary
        ));
        self.app.transcript.notice(format!(
            "approved once: {} {}; the agent is told it may retry",
            call.tool, call.summary
        ));
    }

    /// The human's answer to an unavailable guardian: the session is changed first, then the
    /// waiting call is told what to do.
    pub(crate) async fn answer_unavailable(
        &mut self,
        runtime: &AgentRuntime,
        answer: UnavailableAnswer,
    ) {
        let choice = match answer {
            UnavailableAnswer::Retry => match runtime.retry_guardian().await {
                Ok((true, notice)) => {
                    if let Some(notice) = notice {
                        self.app.transcript.warning(notice, None);
                    }
                    UnavailableChoice::Retry
                }
                Ok((false, _)) => {
                    self.app
                        .transcript
                        .error("the guardian model is still unavailable");
                    UnavailableChoice::Deny
                }
                Err(e) => {
                    self.app
                        .transcript
                        .error(fontaine_tools::error_chain::chain(&e));
                    UnavailableChoice::Deny
                }
            },
            UnavailableAnswer::Pick(reference) => match runtime.use_guardian(&reference).await {
                Ok(Some(switched)) => {
                    self.remember_model(PickerPurpose::Guardian, &switched.reference);
                    self.finish_switch(
                        format!("guardian is now {}", switched.reference),
                        switched.persist_error,
                        overriding_scope(&runtime.config, "guardian.model"),
                    );
                    if let Some(notice) = switched.facts_notice {
                        self.app.transcript.warning(notice, None);
                    }
                    UnavailableChoice::Retry
                }
                Ok(None) => {
                    self.app
                        .transcript
                        .error(format!("{reference} is unavailable too"));
                    UnavailableChoice::Deny
                }
                Err(e) => {
                    self.app
                        .transcript
                        .error(fontaine_tools::error_chain::chain(&e));
                    UnavailableChoice::Deny
                }
            },
            UnavailableAnswer::Manual => {
                self.app
                    .transcript
                    .notice("switched this session to manual mode");
                UnavailableChoice::Manual
            }
            UnavailableAnswer::Deny => UnavailableChoice::Deny,
        };
        self.app.finish_unavailable(choice);
    }
}

fn takes(levels: &[Effort]) -> String {
    if levels.is_empty() {
        return "no effort level (the endpoint has no reasoning mapping)".to_string();
    }
    levels
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_with_no_levels_says_so() {
        assert!(takes(&[]).starts_with("no effort level"));
        assert_eq!(takes(&[Effort::Low, Effort::High]), "low, high");
    }
}
