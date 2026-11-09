//! The `models` tool's source: the session's accounts, listed fresh at call time and marked
//! with the roles the session has then.

use std::sync::{Arc, RwLock};

use fontaine_tools::lock::read;
use fontaine_tools::models_tool::ModelSource;
use futures::future::BoxFuture;

use crate::child_runner::MainModel;
use crate::model_report::ModelReport;
use crate::models::{ModelRef, SharedSwitcher, validated_models};

/// Where the guardian's model is read from at call time.
pub type GuardianRef = Arc<dyn Fn() -> Option<ModelRef> + Send + Sync>;

pub struct SessionModels {
    /// The session's model roles, one line each, listed after the accounts.
    pub roles: String,
    pub switcher: Option<SharedSwitcher>,
    pub main: Arc<RwLock<MainModel>>,
    pub guardian: GuardianRef,
}

impl ModelSource for SessionModels {
    fn report<'a>(&'a self, account: Option<&'a str>) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            // A copy, so the lock is not held across the listing.
            let switcher = self
                .switcher
                .as_ref()
                .map(|switcher| read(switcher, "model switcher").clone())
                .ok_or("models cannot be listed in this session")?;
            let only = match account {
                Some(name) if !switcher.config.accounts.contains_key(name) => {
                    let known: Vec<&str> = switcher
                        .config
                        .accounts
                        .keys()
                        .map(String::as_str)
                        .collect();
                    return Err(format!(
                        "unknown account `{name}`; configured accounts: {}",
                        known.join(", ")
                    ));
                }
                Some(name) => Some(vec![name.to_string()]),
                None => None,
            };
            let (main_ref, main_listing) = {
                let main = read(&self.main, "main model");
                (main.handle.reference.clone(), main.listing.clone())
            };
            let guardian = (self.guardian)();
            let listings = switcher
                .listings(Some((&main_ref, &main_listing)), only.as_deref())
                .await;
            let mut text = ModelReport::build(
                &listings,
                Some(&main_ref),
                guardian.as_ref(),
                &validated_models(),
            )
            .render_text();
            if !self.roles.is_empty() {
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str("\nRoles (a role name is accepted wherever a model is):\n");
                text.push_str(&self.roles);
                text.push('\n');
            }
            Ok(text)
        })
    }
}
