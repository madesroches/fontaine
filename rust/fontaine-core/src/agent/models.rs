//! Switching the main and guardian models, the model listings, and keeping both in line with
//! accounts and credentials that change mid-session.

use std::path::Path;
use std::sync::{Arc, RwLock};

use fontaine_tools::error_chain::chain;
use fontaine_tools::lock::{read, write};
use rig_core::completion::GenerationOptions;

use super::AgentRuntime;
use super::setup::{build_guardian_telling, warm_output_mode};
use crate::accounts::AccountsError;
use crate::child_runner::MainModel;
use crate::config::Account;
use crate::config::MainSource;
use crate::model_roles::{ModelChain, Tried, fall_through_notice};
use crate::models::{
    AccountListing, GuardianChoice, GuardianModel, Listing, ModelHandle, ModelRef, Switcher,
};
use crate::quota::QuotaSource;
use crate::runtime_error::RuntimeError;
use crate::scanner_stage::llm_backend;
use crate::usage_ledger::Served;
use crate::user_config;

/// Whether role `name` is `main` or lists, through other roles, `main` (or `$main`): saving
/// its name as `[model] model` would fold into `main` and make it list itself.
fn reaches_main(roles: &std::collections::BTreeMap<String, Vec<String>>, name: &str) -> bool {
    let mut seen = vec![name];
    let mut pending = vec![name];
    while let Some(role) = pending.pop() {
        if role == crate::model_roles::MAIN_ROLE {
            return true;
        }
        for entry in roles.get(role).into_iter().flatten() {
            let target = if entry == "$main" {
                crate::model_roles::MAIN_ROLE
            } else {
                entry.as_str()
            };
            if !entry.contains('/') && !seen.contains(&target) {
                seen.push(target);
                pending.push(target);
            }
        }
    }
    false
}

/// A guardian account's name and the source of its subscription quota.
pub type GuardianQuota = (String, Arc<dyn QuotaSource>);

pub(super) fn guardian_quota_of(model: Option<&GuardianModel>) -> Option<GuardianQuota> {
    match model {
        Some(GuardianModel::Ready(handle)) => handle
            .quota
            .as_ref()
            .map(|source| (handle.reference.account.clone(), source.clone())),
        _ => None,
    }
}

pub(super) fn guardian_quota_slot(
    model: Option<&GuardianModel>,
) -> Arc<RwLock<Option<GuardianQuota>>> {
    Arc::new(RwLock::new(guardian_quota_of(model)))
}

/// What a model switch did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Switched {
    pub reference: String,
    /// The guardian moved with the main model because it was on the main-model fallback.
    pub guardian_followed: bool,
    /// Why the choice could not be saved, when it could not.
    pub persist_error: Option<String>,
    /// The installed guardian's facts warning, set where the runtime's own record of the
    /// guardian is not updated and the caller cannot read it from there.
    pub facts_notice: Option<String>,
    /// Why the choice was deliberately not saved, naming the setting to edit instead.
    pub session_only: Option<String>,
}

/// A main model list that moved to its next entry after the serving one ran out of quota.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FellThrough {
    pub from: ModelRef,
    pub to: ModelRef,
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
    /// The facts warning of the guardian the retry installed, when it is a derived model.
    pub guardian_notice: Option<String>,
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
        *write(&self.guardian_quota, "guardian quota") = guardian_quota_of(Some(model));
        if let Some(slot) = &self.llm_scanner {
            let timeout = std::time::Duration::from_secs(self.config.guardian.timeout_secs);
            *write(slot, "scanner model") = llm_backend(Some(model), timeout);
        }
    }

    /// A copy of the shared switcher, so no lock is held across an await.
    pub(super) fn switcher(&self) -> Result<Switcher, RuntimeError> {
        self.setup
            .switcher
            .as_ref()
            .map(|switcher| read(switcher, "model switcher").clone())
            .ok_or_else(|| RuntimeError::new("models cannot be switched in this session"))
    }

    async fn guardian_for(&self, text: &str) -> Result<GuardianModel, RuntimeError> {
        self.switcher()?
            .guardian(
                text,
                Some((
                    &self.models.main.reference,
                    &self.models.main_listing.models,
                )),
            )
            .await
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
    /// choice as `[model] model`, unless the user's settings give the main model as a role or a
    /// list, which a typed choice must not overwrite. The guardian follows only when it is on the
    /// main-model fallback; a pinned or validated guardian stays where it is.
    pub async fn switch_main_model(&mut self, text: &str) -> Result<Switched, RuntimeError> {
        let text = text.trim();
        let guardian_followed = self.install_main_chain(text).await?;
        // The level stays; a new model that cannot take it is told about now, not at the next
        // turn.
        self.turn_generation_options();
        let reference = self.models.main.reference.to_string();
        let (persist_error, session_only) = match self.main_choice_persistence(text) {
            Ok(None) => (
                self.persist(|path| user_config::set_model(path, text))
                    .map(|e| chain(&e)),
                None,
            ),
            Ok(Some(why)) => (None, Some(why)),
            Err(e) => (Some(chain(&e)), None),
        };
        Ok(Switched {
            reference,
            guardian_followed,
            persist_error,
            facts_notice: None,
            session_only,
        })
    }

    /// Whether a typed `/model` choice may be saved as `[model] model`: `None` when it may, else
    /// why it stays with this session. A role or a list in the settings is the user's own
    /// document, and a second `main` key beside it would stop the next start.
    fn main_choice_persistence(&self, text: &str) -> Result<Option<String>, RuntimeError> {
        use fontaine_agents::registry::ModelSpec;
        match ModelSpec::parse(text) {
            Ok(ModelSpec::Model(_)) => {}
            Ok(ModelSpec::Named(name)) => {
                if reaches_main(&self.config.roles, &name) {
                    return Ok(Some(format!(
                        "`{name}` reaches `main`, so saving it as `[model] model` would make the roles loop; edit your settings to change it for later sessions"
                    )));
                }
            }
            Ok(ModelSpec::Main) => {
                return Ok(Some(
                    "`$main` is already the configured main model".to_string(),
                ));
            }
            _ => {
                return Ok(Some(
                    "a list is not saved; set `[model] model` to it in your settings".to_string(),
                ));
            }
        }
        let held = match self.config.main_source {
            MainSource::Role => Some("`main` under `[models.roles]`"),
            MainSource::ModelKeyList => Some("`[model] model`, which is a list"),
            MainSource::Unset | MainSource::ModelKeyString => None,
        };
        if let Some(held) = held {
            return Ok(Some(format!(
                "the main model is set by {held}; edit it there to change it for later sessions"
            )));
        }
        let Some(path) = &self.setup.user_config else {
            return Ok(None);
        };
        let shape = user_config::main_model_shape(path)
            .map_err(|e| RuntimeError::caused("could not read the settings file", e))?;
        Ok(match shape {
            user_config::MainShape::Role => Some(
                "the main model is set by `main` under `[models.roles]` in your settings; edit it there to change it for later sessions"
                    .to_string(),
            ),
            user_config::MainShape::List => Some(
                "`[model] model` in your settings is a list; edit it there to change it for later sessions"
                    .to_string(),
            ),
            user_config::MainShape::Other => None,
        })
    }

    /// The role and model the main conversation's calls are recorded against in the usage ledger.
    pub fn served(&self) -> Served {
        Served::new(
            self.models.main_chain.role.as_deref(),
            &self.models.main.reference,
        )
    }

    /// The chain of the role `name`, shared by everything running on it, when the user defined it.
    pub fn role_chain(&self, name: &str) -> Option<Arc<ModelChain>> {
        self.setup
            .role_chains
            .chain_of_role(&self.setup.roles, name)
    }

    /// The switcher and the chain of the role `name`, for a one-shot job that outlives a borrow of
    /// the runtime. `None` when the user did not define the role or no switcher is available.
    pub fn job_chain(&self, name: &str) -> Option<(Switcher, Arc<ModelChain>)> {
        Some((self.switcher().ok()?, self.role_chain(name)?))
    }

    /// Records a one-shot job's move to the next entry of its list, and says so.
    pub(super) fn announce_fall_through(
        &self,
        agent: &str,
        subject: &str,
        chain: &ModelChain,
        from: &ModelRef,
        to: &ModelRef,
        class: &str,
    ) {
        self.state.record_model_fall_through(
            agent,
            chain.role.as_deref(),
            &from.to_string(),
            &to.to_string(),
            class,
        );
        self.approver
            .warn(&fall_through_notice(subject, from, to, class));
    }

    /// The reasoning effort the next main turn sends, checked against the current model. A level
    /// the model cannot take is left out and the human told once.
    pub fn turn_generation_options(&self) -> Option<GenerationOptions> {
        self.models
            .main
            .generation_options_warned(&self.state, self.state.effort())
    }

    /// Resolves the first entry of the list `text` names, rebuilds the agent on it and makes the
    /// list the session's main chain and its entry the model of the session and its subagents,
    /// without saving anything. Returns whether the guardian followed.
    async fn install_main_chain(&mut self, text: &str) -> Result<bool, RuntimeError> {
        let switcher = self.switcher()?;
        let list = Arc::new(
            switcher
                .main_chain(text)
                .map_err(|e| RuntimeError::caused("could not resolve the model", e))?,
        );
        let (handle, listing) = switcher
            .main(&list.entries[0].to_string())
            .await
            .map_err(|e| RuntimeError::caused("could not resolve the model", e))?;
        self.install_handle(handle, listing, Some(list))
    }

    /// Resolves entry `index` of the session's main chain and rebuilds the agent on it, keeping
    /// the chain. Does not move the chain's active entry.
    async fn install_main_entry(&mut self, index: usize) -> Result<bool, RuntimeError> {
        let switcher = self.switcher()?;
        let reference = self.models.main_chain.entries[index].clone();
        let (handle, listing) = switcher
            .main(&reference.to_string())
            .await
            .map_err(|e| RuntimeError::caused("could not resolve the model", e))?;
        self.install_handle(handle, listing, None)
    }

    /// Makes `handle` the model of the session, and `chain` its list when it starts a new one.
    fn install_handle(
        &mut self,
        handle: ModelHandle,
        listing: Listing,
        chain: Option<Arc<ModelChain>>,
    ) -> Result<bool, RuntimeError> {
        let previous = std::mem::replace(&mut self.models.main, handle);
        let previous_listing = std::mem::replace(&mut self.models.main_listing, listing);
        if let Err(e) = self.rebuild() {
            self.models.main = previous;
            self.models.main_listing = previous_listing;
            return Err(e);
        }
        if let Some(chain) = chain {
            self.models.main_chain = chain;
        }
        *write(&self.main, "main model") = MainModel {
            handle: self.models.main.clone(),
            chain: self.models.main_chain.clone(),
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

    /// The main model ran out of quota: moves the session to the next entry of its list that can
    /// serve, rebuilding the agent on it, and warns. The entry becomes the one later turns start
    /// on. `tried` holds the entries this request has already failed on, which are not tried
    /// again. `None` when the list is spent: every other entry failed, failed to resolve, was
    /// skipped under a cost cap, or there is none.
    pub async fn fall_through(&mut self, class: &str, tried: &mut Tried) -> Option<FellThrough> {
        let list = self.models.main_chain.clone();
        let failed = list.active();
        let from = list.entries[failed].clone();
        let switcher = self.switcher().ok()?;
        for index in tried.order(&list, failed) {
            let to = list.entries[index].clone();
            let resolved = switcher.main(&to.to_string()).await;
            let (handle, listing) = match resolved {
                Ok(resolved) => resolved,
                Err(e) => {
                    self.approver.warn(&format!("skipping {to}: {}", chain(&e)));
                    continue;
                }
            };
            if self.setup.explicit_budget_cap && !handle.priced() {
                continue;
            }
            if let Err(e) = self.install_handle(handle, listing, None) {
                self.approver.warn(&format!("skipping {to}: {}", chain(&e)));
                continue;
            }
            list.settle(index);
            tried.note(index);
            self.announce_fall_through(
                fontaine_tools::gate::ROOT_AGENT,
                list.role.as_deref().unwrap_or("main"),
                &list,
                &from,
                &to,
                class,
            );
            return Some(FellThrough { from, to });
        }
        None
    }

    /// What to add to a failure when the main model's list has nothing left to fall through to.
    pub fn fall_through_spent(&self) -> Option<String> {
        let list = &self.models.main_chain;
        (list.entries.len() > 1).then(|| format!("every model in {} failed", list.label()))
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
            slot.models = Ok(self.models.main_listing.models.clone());
            slot.refused = self.models.main_listing.refused.clone();
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
    /// the guardian's composition uses it.
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
            .composition
            .as_ref()
            .is_some_and(|composition| composition.models().iter().any(|model| on(model)))
        {
            return Err(AccountsError::InUse {
                by: "the guardian's composition",
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
            let at = self
                .models
                .main_chain
                .entries
                .iter()
                .position(|entry| *entry == self.models.main.reference)
                .unwrap_or_else(|| self.models.main_chain.active());
            match self.install_main_entry(at).await {
                Ok(_) => effect.main_rebuilt = true,
                Err(e) => effect.main_error = Some(chain(&e)),
            }
        }
        let guardian_on_account = ModelRef::parse(self.state.guardian().label())
            .is_some_and(|reference| reference.account == name);
        if guardian_on_account {
            let (ready, notice) = self.retry_guardian().await.unwrap_or((false, None));
            effect.guardian_ready = Some(ready);
            effect.guardian_notice = notice;
        }
        effect
    }

    /// `/guardian-model`: pins the guardian to `text` for the live session and saves it as
    /// `[guardian] model`. An account with no credential gives an unavailable guardian rather
    /// than an error, which the next adjudication reports.
    pub async fn set_guardian_model(&mut self, text: &str) -> Result<Switched, RuntimeError> {
        let model = self.guardian_for(text).await?;
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
            facts_notice: None,
            session_only: None,
        })
    }

    /// Tries the guardian model again, for when its credential has appeared since startup.
    /// Returns whether it can be asked now, and the facts warning of the model it installed.
    pub async fn retry_guardian(&self) -> Result<(bool, Option<String>), RuntimeError> {
        let current = self.state.guardian();
        let model = self.guardian_for(current.label()).await?;
        self.install_guardian(&model);
        Ok(match &model {
            GuardianModel::Ready(handle) => (true, handle.facts_notice.clone()),
            _ => (false, None),
        })
    }

    /// The "pick another" answer to an unavailable guardian: switches the guardian and, when it
    /// can be asked, saves it as `[guardian] model`. `None` when the pick is unavailable too,
    /// which is not saved so the next session does not open on it.
    ///
    /// It takes `&self` because it answers a prompt raised mid-turn, so the runtime's own record
    /// of the guardian is not updated; the live guardian is the gate state's.
    pub async fn use_guardian(&self, text: &str) -> Result<Option<Switched>, RuntimeError> {
        let model = self.guardian_for(text).await?;
        self.install_guardian(&model);
        let GuardianModel::Ready(handle) = &model else {
            return Ok(None);
        };
        let facts_notice = handle.facts_notice.clone();
        let reference = model.reference().to_string();
        let persist_error = self
            .persist(|path| user_config::set_guardian_model(path, &reference))
            .map(|e| chain(&e));
        Ok(Some(Switched {
            reference,
            guardian_followed: false,
            persist_error,
            facts_notice,
            session_only: None,
        }))
    }

    /// Guardian models to offer in place of an unavailable one.
    pub fn guardian_candidates(&self) -> Vec<ModelRef> {
        self.switcher().map_or_else(
            |_| Vec::new(),
            |switcher| {
                switcher.candidates(
                    &self.models.main.reference,
                    &self.models.main_listing.models,
                )
            },
        )
    }
}

#[cfg(test)]
mod reaches_main_tests {
    use super::reaches_main;
    use std::collections::BTreeMap;

    #[test]
    fn a_role_listing_main_would_loop_when_saved_as_the_main_model() {
        let roles = BTreeMap::from([
            (
                "thinking".to_string(),
                vec!["$main".to_string(), "b/y".to_string()],
            ),
            ("plain".to_string(), vec!["b/y".to_string()]),
            ("outer".to_string(), vec!["thinking".to_string()]),
        ]);
        assert!(reaches_main(&roles, "main"));
        assert!(reaches_main(&roles, "thinking"));
        assert!(reaches_main(&roles, "outer"));
        assert!(!reaches_main(&roles, "plain"));
    }
}
