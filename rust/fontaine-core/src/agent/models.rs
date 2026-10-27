//! Switching the main and guardian models, the model listings, and keeping both in line with
//! accounts and credentials that change mid-session.

use std::path::Path;
use std::sync::Arc;

use fontaine_tools::error_chain::chain;
use fontaine_tools::lock::{read, write};

use super::AgentRuntime;
use super::setup::{build_guardian_telling, warm_output_mode};
use crate::accounts::AccountsError;
use crate::child_runner::MainModel;
use crate::config::Account;
use crate::models::{AccountListing, GuardianChoice, GuardianModel, ModelRef, Switcher};
use crate::runtime_error::RuntimeError;
use crate::scanner_stage::llm_backend;
use crate::user_config;

/// What a model switch did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Switched {
    pub reference: String,
    /// The guardian moved with the main model because it was on the main-model fallback.
    pub guardian_followed: bool,
    /// Why the choice could not be saved, when it could not.
    pub persist_error: Option<String>,
}

/// What a credential change did to the live session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CredentialEffect {
    /// The main model was on the account and was rebuilt on its current credential.
    pub main_rebuilt: bool,
    /// The main model is on the account but could not be rebuilt; it keeps the client it was
    /// built with.
    pub main_error: Option<String>,
    /// The guardian is on the account: whether it can be asked now.
    pub guardian_ready: Option<bool>,
}

impl AgentRuntime {
    /// Makes `model` the guardian from the next adjudication on. Its verdict cache starts empty.
    pub fn install_guardian(&self, model: &GuardianModel) {
        let guardian = Arc::new(build_guardian_telling(
            Some(model),
            &self.config,
            self.guardian_profile,
            Some(&self.approver),
        ));
        warm_output_mode(&guardian);
        self.state.set_guardian(guardian);
        if let Some(slot) = &self.llm_scanner {
            let timeout = std::time::Duration::from_secs(self.config.guardian.timeout_secs);
            *write(slot, "scanner model") = llm_backend(Some(model), timeout);
        }
    }

    /// A copy of the shared switcher, so no lock is held across an await.
    fn switcher(&self) -> Result<Switcher, RuntimeError> {
        self.setup
            .switcher
            .as_ref()
            .map(|switcher| read(switcher, "model switcher").clone())
            .ok_or_else(|| RuntimeError::new("models cannot be switched in this session"))
    }

    fn guardian_for(&self, text: &str) -> Result<GuardianModel, RuntimeError> {
        self.switcher()?
            .guardian(
                text,
                Some((&self.models.main.reference, &self.models.main_listing)),
            )
            .map_err(|e| RuntimeError::caused("could not resolve the guardian model", e))
    }

    fn persist(&self, write: impl FnOnce(&Path) -> std::io::Result<()>) -> Option<RuntimeError> {
        match &self.setup.user_config {
            Some(path) => write(path).err().map(|e| {
                RuntimeError::caused(format!("could not save it to {}", path.display()), e)
            }),
            None => Some(RuntimeError::new(
                "there is no user settings file to save it to",
            )),
        }
    }

    /// `/model`: switches the live session's model, rebuilds the agent on it and saves the
    /// choice as `[model] model`. The guardian follows only when it is on the main-model
    /// fallback; a pinned or validated guardian stays where it is.
    pub async fn switch_main_model(&mut self, text: &str) -> Result<Switched, RuntimeError> {
        let guardian_followed = self.install_main(text).await?;
        let reference = self.models.main.reference.to_string();
        let persist_error = self
            .persist(|path| user_config::set_model(path, &reference))
            .map(|e| chain(&e));
        Ok(Switched {
            reference,
            guardian_followed,
            persist_error,
        })
    }

    /// Resolves `text`, rebuilds the agent on it and makes it the model of the session and its
    /// subagents, without saving anything. Returns whether the guardian followed.
    async fn install_main(&mut self, text: &str) -> Result<bool, RuntimeError> {
        let (handle, listing) = self
            .switcher()?
            .main(text)
            .await
            .map_err(|e| RuntimeError::caused("could not resolve the model", e))?;
        let previous = std::mem::replace(&mut self.models.main, handle);
        let previous_listing = std::mem::replace(&mut self.models.main_listing, listing);
        if let Err(e) = self.rebuild() {
            self.models.main = previous;
            self.models.main_listing = previous_listing;
            return Err(e);
        }
        *write(&self.main, "main model") = MainModel {
            handle: self.models.main.clone(),
            listing: self.models.main_listing.clone(),
        };
        self.state
            .agents()
            .set_root_model(self.models.main.reference.to_string());
        let guardian_followed = self.models.guardian_choice == GuardianChoice::MainFallback;
        if guardian_followed {
            let model = GuardianModel::Ready(self.models.main.guardian_scoped());
            self.install_guardian(&model);
            self.models.guardian = Some(model);
        }
        Ok(guardian_followed)
    }

    /// The models of every configured account, for the `/model` and `/guardian-model` pickers. The first call fetches
    /// them; later calls fetch only the accounts that failed before or were not listed yet, so
    /// a credential or account added mid-session shows up while cached successes make no
    /// request. The main account's entry always follows the current listing. With `account`,
    /// that account is fetched whether cached or not and only its listing is returned.
    pub async fn model_listings(
        &mut self,
        account: Option<&str>,
    ) -> Result<Vec<AccountListing>, RuntimeError> {
        let switcher = self.switcher()?;
        let wanted: Option<Vec<String>> = match account {
            Some(name) => Some(vec![name.to_string()]),
            None if self.listings.is_empty() => None,
            None => Some(
                switcher
                    .config
                    .accounts
                    .keys()
                    .filter(|name| {
                        self.listings
                            .iter()
                            .find(|l| l.account == **name)
                            .is_none_or(|l| l.models.is_err())
                    })
                    .cloned()
                    .collect(),
            ),
        };
        let fetched = switcher
            .listings(
                Some((&self.models.main.reference, &self.models.main_listing)),
                wanted.as_deref(),
            )
            .await;
        for listing in fetched {
            match self
                .listings
                .iter_mut()
                .find(|l| l.account == listing.account)
            {
                Some(slot) => *slot = listing,
                None => self.listings.push(listing),
            }
        }
        self.listings.sort_by(|a, b| a.account.cmp(&b.account));
        let main = &self.models.main.reference.account;
        if let Some(slot) = self.listings.iter_mut().find(|l| l.account == *main) {
            slot.models = Ok(self.models.main_listing.clone());
        }
        Ok(match account {
            Some(name) => self
                .listings
                .iter()
                .filter(|l| l.account == name)
                .cloned()
                .collect(),
            None => self.listings.clone(),
        })
    }

    /// Installs an account the provider dialog added, in the session's configuration and in the
    /// switcher the subagents share. It is listed by the next [`AgentRuntime::model_listings`].
    pub fn account_added(&mut self, name: &str, account: Account) {
        self.config
            .accounts
            .insert(name.to_string(), account.clone());
        if let Some(switcher) = &self.setup.switcher {
            write(switcher, "model switcher")
                .config
                .accounts
                .insert(name.to_string(), account);
        }
    }

    /// Forgets an account the provider dialog removed.
    pub fn account_removed(&mut self, name: &str) {
        self.config.accounts.remove(name);
        if let Some(switcher) = &self.setup.switcher {
            write(switcher, "model switcher")
                .config
                .accounts
                .remove(name);
        }
        self.listings.retain(|l| l.account != name);
    }

    /// Whether the account may be removed: not while the live main model, the live guardian or
    /// the screen stage uses it.
    pub fn removable(&self, name: &str) -> Result<(), AccountsError> {
        crate::accounts::check_defined_only_in_user_file(&self.config, name)?;
        let on = |reference: &str| ModelRef::parse(reference).is_some_and(|r| r.account == name);
        if self.models.main.reference.account == name {
            return Err(AccountsError::InUse {
                by: "the main model",
            });
        }
        if on(self.state.guardian().label()) {
            return Err(AccountsError::InUse { by: "the guardian" });
        }
        if self
            .config
            .guardian
            .screen
            .as_ref()
            .is_some_and(|screen| on(&screen.model))
        {
            return Err(AccountsError::InUse {
                by: "the guardian's screen stage",
            });
        }
        Ok(())
    }

    /// Brings the session in line with a credential stored, replaced or removed for `name`: its
    /// cached listing is dropped, the main model is rebuilt when it is on the account, and the
    /// guardian is tried again when it is. A missing or invalid credential leaves the guardian
    /// unavailable, which fails closed. Nothing is saved to the settings.
    pub async fn credentials_changed(&mut self, name: &str) -> CredentialEffect {
        self.listings.retain(|l| l.account != name);
        let mut effect = CredentialEffect::default();
        if self.models.main.reference.account == name {
            let reference = self.models.main.reference.to_string();
            match self.install_main(&reference).await {
                Ok(_) => effect.main_rebuilt = true,
                Err(e) => effect.main_error = Some(chain(&e)),
            }
        }
        let guardian_on_account = ModelRef::parse(self.state.guardian().label())
            .is_some_and(|reference| reference.account == name);
        if guardian_on_account {
            effect.guardian_ready = Some(self.retry_guardian().unwrap_or(false));
        }
        effect
    }

    /// `/guardian-model`: pins the guardian to `text` for the live session and saves it as
    /// `[guardian] model`. An account with no credential gives an unavailable guardian rather
    /// than an error, which the next adjudication reports.
    pub fn set_guardian_model(&mut self, text: &str) -> Result<Switched, RuntimeError> {
        let model = self.guardian_for(text)?;
        self.install_guardian(&model);
        let reference = model.reference().to_string();
        self.models.guardian = Some(model);
        self.models.guardian_choice = GuardianChoice::Pinned;
        let persist_error = self
            .persist(|path| user_config::set_guardian_model(path, &reference))
            .map(|e| chain(&e));
        Ok(Switched {
            reference,
            guardian_followed: false,
            persist_error,
        })
    }

    /// Tries the guardian model again, for when its credential has appeared since startup.
    /// Returns whether it can be asked now.
    pub fn retry_guardian(&self) -> Result<bool, RuntimeError> {
        let current = self.state.guardian();
        let model = self.guardian_for(current.label())?;
        self.install_guardian(&model);
        Ok(matches!(model, GuardianModel::Ready(_)))
    }

    /// The "pick another" answer to an unavailable guardian: switches the guardian and, when it
    /// can be asked, saves it as `[guardian] model`. `None` when the pick is unavailable too,
    /// which is not saved so the next session does not open on it.
    ///
    /// It takes `&self` because it answers a prompt raised mid-turn, so the runtime's own record
    /// of the guardian is not updated; the live guardian is the gate state's.
    pub fn use_guardian(&self, text: &str) -> Result<Option<Switched>, RuntimeError> {
        let model = self.guardian_for(text)?;
        self.install_guardian(&model);
        if !matches!(model, GuardianModel::Ready(_)) {
            return Ok(None);
        }
        let reference = model.reference().to_string();
        let persist_error = self
            .persist(|path| user_config::set_guardian_model(path, &reference))
            .map(|e| chain(&e));
        Ok(Some(Switched {
            reference,
            guardian_followed: false,
            persist_error,
        }))
    }

    /// Guardian models to offer in place of an unavailable one.
    pub fn guardian_candidates(&self) -> Vec<ModelRef> {
        self.switcher().map_or_else(
            |_| Vec::new(),
            |switcher| switcher.candidates(&self.models.main.reference, &self.models.main_listing),
        )
    }
}
