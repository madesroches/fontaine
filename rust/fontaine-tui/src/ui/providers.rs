use std::collections::VecDeque;

use crate::config_panel::ConfigKey;
use crate::provider_dialog::{
    DialogKey, Outcome, ProviderDialog, Secret, log_out_summary, sign_in_failure,
    stored_key_summary,
};
use crate::{Action, App, Ui};
use fontaine_core::accounts::{self, AccountsError, NewAccount};
use fontaine_core::agent::AgentRuntime;
use tokio::sync::mpsc::unbounded_channel;

/// Queues a dialog request for the idle loop. A cancel takes the sign-in it cancels out of the
/// queue, so a sign-in the human already backed out of never starts.
pub(crate) fn queue_provider_request(queue: &mut VecDeque<DialogKey>, request: DialogKey) {
    if request == DialogKey::CancelSignIn {
        if let Some(at) = queue
            .iter()
            .rposition(|r| matches!(r, DialogKey::SignIn(_)))
        {
            queue.remove(at);
        }
        return;
    }
    queue.push_back(request);
}

impl<R: FnMut(&mut App)> Ui<R> {
    /// `/login [account]`: opens the provider dialog, on an account's log-in when one is named.
    pub(crate) async fn login(&mut self, runtime: &mut AgentRuntime, target: &str) {
        let Some(rows) = self.account_rows(runtime) else {
            return;
        };
        let (dialog, request) = ProviderDialog::new(rows, Some(target));
        self.app.providers = Some(dialog);
        self.provider_request(runtime, request).await;
    }

    fn account_rows(&mut self, runtime: &AgentRuntime) -> Option<Vec<accounts::AccountRow>> {
        match accounts::rows(&runtime.config, &self.data_dir, &*self.env) {
            Ok(rows) => Some(rows),
            Err(e) => {
                self.app.transcript.error(format!(
                    "cannot list the accounts: {}",
                    fontaine_tools::error_chain::chain(&e)
                ));
                None
            }
        }
    }

    pub(crate) fn queue_provider(&mut self, request: DialogKey) {
        queue_provider_request(&mut self.pending_provider, request);
    }

    /// Gives the dialog a result; what it asks for next comes back.
    fn tell_dialog(&mut self, outcome: Outcome) -> DialogKey {
        self.app
            .providers
            .as_mut()
            .map_or(DialogKey::None, |dialog| dialog.finish(outcome))
    }

    fn refresh_rows(&mut self, runtime: &AgentRuntime) {
        if let Some(rows) = self.account_rows(runtime) {
            self.tell_dialog(Outcome::Rows(rows));
        }
    }

    /// Carries out what the dialog asked for, and what its answer asks for in turn.
    pub(crate) async fn provider_request(
        &mut self,
        runtime: &mut AgentRuntime,
        mut request: DialogKey,
    ) {
        loop {
            let next = match request {
                DialogKey::None | DialogKey::Close | DialogKey::CancelSignIn => return,
                DialogKey::Add(new) => self.add_account(runtime, &new),
                DialogKey::StoreKey { account, key } => {
                    self.store_key(runtime, &account, key).await
                }
                DialogKey::SignIn(account) => self.sign_in(runtime, &account).await,
                DialogKey::LogOut(account) => self.log_out(runtime, &account).await,
                DialogKey::Remove(account) => self.remove_account(runtime, &account),
                DialogKey::PickModel(account) => {
                    self.app.providers = None;
                    self.open_model_picker(runtime, Some(&account)).await;
                    return;
                }
            };
            if next == DialogKey::None {
                return;
            }
            request = next;
        }
    }

    fn failed(&mut self, error: &(dyn std::error::Error + 'static)) -> DialogKey {
        self.tell_dialog(Outcome::Failed(fontaine_tools::error_chain::chain(error)))
    }

    fn add_account(&mut self, runtime: &mut AgentRuntime, new: &NewAccount) -> DialogKey {
        let Some(user_file) = self.user_config.clone() else {
            return self.tell_dialog(Outcome::Failed(
                "there is no user settings file to add the account to".to_string(),
            ));
        };
        match accounts::add(&user_file, &self.data_dir, new, &runtime.config) {
            Ok(account) => {
                runtime.account_added(&new.name, account);
                self.app.transcript.notice(format!(
                    "added account `{}` to {}",
                    new.name,
                    user_file.display()
                ));
                self.refresh_rows(runtime);
                self.tell_dialog(Outcome::Added {
                    account: new.name.clone(),
                })
            }
            Err(e) => self.failed(&e),
        }
    }

    /// What a changed credential did to the session, as text to append to the summary line.
    async fn credential_effects(
        &mut self,
        runtime: &mut AgentRuntime,
        account: &str,
        logged_out: bool,
    ) -> String {
        let effect = runtime.credentials_changed(account).await;
        let mut text = String::new();
        if effect.main_rebuilt {
            text.push_str(if logged_out {
                "; the main model was rebuilt without the stored credential"
            } else {
                "; the main model now uses it"
            });
        }
        if let Some(why) = effect.main_error {
            text.push_str(&format!(
                "; the main model keeps the credential it started with until you switch models or restart ({why})"
            ));
        }
        match effect.guardian_ready {
            Some(true) => text.push_str("; the guardian is ready"),
            Some(false) => text.push_str(
                "; the guardian is unavailable, so calls that need it are denied until it has a credential",
            ),
            None => {}
        }
        text
    }

    async fn store_key(
        &mut self,
        runtime: &mut AgentRuntime,
        account: &str,
        key: Secret,
    ) -> DialogKey {
        let stored = accounts::store_key(&self.data_dir, account, key.expose());
        drop(key);
        if let Err(e) = stored {
            return self.failed(&e);
        }
        let effects = self.credential_effects(runtime, account, false).await;
        let api_key_env = runtime
            .config
            .accounts
            .get(account)
            .and_then(|a| a.api_key_env.as_deref());
        let summary = stored_key_summary(account, &effects, api_key_env);
        self.finish_login(runtime, account, summary)
    }

    fn finish_login(
        &mut self,
        runtime: &AgentRuntime,
        account: &str,
        summary: String,
    ) -> DialogKey {
        self.app.transcript.notice(summary.clone());
        self.refresh_rows(runtime);
        self.tell_dialog(Outcome::LoggedIn {
            account: account.to_string(),
            summary,
        })
    }

    /// Signs in while still serving keys and prompts: Esc in the dialog drops the sign-in.
    async fn sign_in(&mut self, runtime: &mut AgentRuntime, name: &str) -> DialogKey {
        let Some(account) = runtime.config.accounts.get(name).cloned() else {
            return self.tell_dialog(Outcome::Failed(format!("unknown account `{name}`")));
        };
        let network = match fontaine_core::models::account_network(&runtime.config, name) {
            Ok(network) => network,
            Err(reason) => return self.tell_dialog(Outcome::Failed(reason)),
        };
        let (steps_tx, mut steps) = unbounded_channel();
        let progress = move |step| {
            let _ = steps_tx.send(step);
        };
        let data_dir = self.data_dir.clone();
        let result = {
            let mut run = Box::pin(accounts::sign_in(
                name, &account, &data_dir, &network, &progress,
            ));
            loop {
                self.draw(runtime);
                tokio::select! {
                    biased;
                    result = &mut run => break Some(result),
                    Some(step) = steps.recv() => self.sign_in_step(step),
                    input = self.input.recv() => match input {
                        None => break None,
                        Some(input) => match self.app.handle_input(input) {
                            Action::Providers(DialogKey::CancelSignIn)
                            | Action::Interrupt
                            | Action::Quit => break None,
                            Action::CycleMode => self.cycle_mode(runtime),
                            Action::RetryDenied(index) => self.retry_denied(runtime, index),
                            Action::Guardian(answer) => self.answer_unavailable(runtime, answer),
                            Action::KillAgent(id) => self.kill_agent(runtime, &id),
                            Action::PickModel(purpose, reference) => {
                                self.pending_pick = Some((purpose, reference));
                            }
                            Action::ResumeSession(id) => self.pending_resume = Some(id),
                            Action::Providers(request) => self.queue_provider(request),
                            Action::Config(change) => self.pending_config.push_back(change),
                            Action::OpenPicker(slot) => {
                                self.pending_config.push_back(ConfigKey::OpenPicker(slot));
                            }
                            Action::Server(_)
                            | Action::None
                            | Action::Submit(_)
                            | Action::Queue(_)
                            | Action::EditQueued
                            | Action::Shell(_)
                            | Action::CancelWakeup => {}
                        },
                    },
                    Some(prompt) = self.prompts.recv() => self.on_prompt(Some(runtime), prompt),
                }
            }
        };
        while let Ok(step) = steps.try_recv() {
            self.sign_in_step(step);
        }
        match result {
            Some(Ok(summary)) => {
                let mut summary = summary;
                summary.push_str(&self.credential_effects(runtime, name, false).await);
                self.finish_login(runtime, name, summary)
            }
            Some(Err(e)) => self.tell_dialog(Outcome::Failed(sign_in_failure(e))),
            None => {
                if let Some(dialog) = self.app.providers.as_mut() {
                    dialog.cancel_sign_in();
                }
                self.app.transcript.notice("sign-in cancelled");
                DialogKey::None
            }
        }
    }

    fn sign_in_step(&mut self, step: accounts::SignInStep) {
        if let accounts::SignInStep::Warning(text) = &step {
            self.app.transcript.notice(text.clone());
        }
        self.tell_dialog(Outcome::Step(step));
    }

    async fn log_out(&mut self, runtime: &mut AgentRuntime, account: &str) -> DialogKey {
        let removed = match accounts::log_out(&self.data_dir, account) {
            Ok(removed) => removed,
            Err(e) => return self.failed(&e),
        };
        let mut summary = log_out_summary(account, removed);
        summary.push_str(&self.credential_effects(runtime, account, true).await);
        self.app.transcript.notice(summary.clone());
        self.refresh_rows(runtime);
        self.tell_dialog(Outcome::Done(summary))
    }

    fn remove_account(&mut self, runtime: &mut AgentRuntime, account: &str) -> DialogKey {
        if let Err(e) = runtime.removable(account) {
            return self.failed(&e);
        }
        let Some(user_file) = self.user_config.clone() else {
            return self.tell_dialog(Outcome::Failed(
                "there is no user settings file to remove the account from".to_string(),
            ));
        };
        match accounts::remove(&user_file, &self.data_dir, account) {
            Ok(()) => {}
            Err(e @ AccountsError::CredentialKept { .. }) => {
                runtime.account_removed(account);
                self.refresh_rows(runtime);
                return self.failed(&e);
            }
            Err(e) => return self.failed(&e),
        }
        runtime.account_removed(account);
        self.app
            .transcript
            .notice(format!("removed account `{account}`"));
        self.refresh_rows(runtime);
        self.tell_dialog(Outcome::Done(format!("removed account `{account}`")))
    }
}
