use crate::model_picker::{Group, ModelPicker, PickerPurpose};
use crate::ui::commands::config::overriding_scope;
use crate::{App, Ui, UnavailableAnswer};
use fontaine_core::agent::AgentRuntime;
use fontaine_core::config::Slot;
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
            }
            Err(e) => self.app.transcript.error(format!(
                "/model: {}",
                fontaine_tools::error_chain::chain(&e)
            )),
        }
    }

    /// Lists the models of every account, or of `account` alone, which can take a while, then
    /// opens the picker.
    pub(crate) async fn open_model_picker(
        &mut self,
        runtime: &mut AgentRuntime,
        account: Option<&str>,
    ) {
        self.app.transcript.notice("listing models…");
        self.draw(runtime);
        let groups: Vec<Group> = match runtime.model_listings(account).await {
            Ok(listings) => listings.iter().map(Group::from).collect(),
            Err(e) => {
                self.app.transcript.error(format!(
                    "/model: {}",
                    fontaine_tools::error_chain::chain(&e)
                ));
                return;
            }
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
        match runtime.set_guardian_model(reference) {
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
                    }
                }
            }
            Err(e) => self.app.transcript.error(format!(
                "/guardian-model: {}",
                fontaine_tools::error_chain::chain(&e)
            )),
        }
    }

    /// Lists every account's models live, like `/model`: `set_guardian_model` rejects a pick
    /// that cannot host a guardian, so filtering here would only hide accounts silently.
    pub(crate) async fn open_guardian_picker(&mut self, runtime: &mut AgentRuntime) {
        self.app.transcript.notice("listing models…");
        self.draw(runtime);
        let groups: Vec<Group> = match runtime.model_listings(None).await {
            Ok(listings) => listings.iter().map(Group::from).collect(),
            Err(e) => {
                self.app.transcript.error(format!(
                    "/guardian-model: {}",
                    fontaine_tools::error_chain::chain(&e)
                ));
                return;
            }
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
    pub(crate) fn answer_unavailable(&mut self, runtime: &AgentRuntime, answer: UnavailableAnswer) {
        let choice = match answer {
            UnavailableAnswer::Retry => match runtime.retry_guardian() {
                Ok(true) => UnavailableChoice::Retry,
                Ok(false) => {
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
            UnavailableAnswer::Pick(reference) => match runtime.use_guardian(&reference) {
                Ok(Some(switched)) => {
                    self.remember_model(PickerPurpose::Guardian, &switched.reference);
                    self.finish_switch(
                        format!("guardian is now {}", switched.reference),
                        switched.persist_error,
                        overriding_scope(&runtime.config, "guardian.model"),
                    );
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
