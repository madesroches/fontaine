use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use rig_core::DynModel;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::anthropic::wire::Auth;
use rig_core::providers::anthropic::{Anthropic as Client, AnthropicConfig};

use rig_core::providers::registry::ProviderId;

use std::collections::BTreeMap;

use rig_core::catalog::Catalog;

use super::derived_facts::{facts_as, no_facts_notice, notice, unlisted, unlisted_facts};
use super::{
    AccountClient, CacheScope, ConnectContext, CredentialPolicy, PromptCaching, Provider, vendor,
};
use crate::config::{Account, AnthropicAuth};
use crate::transport::caused_client;
use fontaine_tools::error_chain::chain;

pub struct Anthropic;

impl Provider for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn credential(&self) -> CredentialPolicy {
        CredentialPolicy::Required
    }

    fn default_endpoint(&self) -> Option<&'static str> {
        Some("https://api.anthropic.com")
    }

    // The version every Anthropic hosting accepts; Azure supports only this one.
    fn native_web_search(&self) -> Option<serde_json::Value> {
        Some(super::anthropic_web_search())
    }

    fn accepts_prompt_caching(&self) -> bool {
        true
    }

    fn default_prompt_caching(&self) -> bool {
        true
    }

    fn accepts_auth(&self) -> bool {
        true
    }

    fn accepts_model_facts(&self) -> bool {
        true
    }

    fn validate(&self, account: &Account) -> Result<(), (&'static str, String)> {
        let anthropic = vendor("anthropic");
        for (served, target) in account.model_facts.iter().flatten() {
            if Catalog::builtin().get(anthropic, served).is_some() {
                return Err((
                    "model_facts",
                    format!(
                        "`{served}` is a model the catalog lists, which keeps its own facts; \
                         map only a name the host invented"
                    ),
                ));
            }
            if facts_as(anthropic, anthropic, target, served).is_none() {
                return Err((
                    "model_facts",
                    format!("`{served}` maps to `{target}`, which is not a catalog Claude model"),
                ));
            }
        }
        Ok(())
    }

    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String> {
        let mut config = AnthropicConfig::new(ctx.api_key().unwrap_or_default());
        if let Some(url) = ctx.base_url() {
            config = config.with_base_url(url);
        }
        if ctx.account.auth == Some(AnthropicAuth::Bearer) {
            config.dialect.quirks.auth = Auth::OptionalBearer;
        }
        Ok(Arc::new(Connected {
            client: config.connect(caused_client(ctx.network).map_err(|e| chain(&e))?),
            caching: PromptCaching::of(ctx.account),
            account_name: ctx.account_name.to_string(),
            model_facts: ctx.account.model_facts.clone().unwrap_or_default(),
        }))
    }
}

pub(super) struct Connected {
    client: Client,
    caching: PromptCaching,
    account_name: String,
    model_facts: BTreeMap<String, String>,
}

impl Connected {
    fn model(&self, model: &str, scope: CacheScope) -> DynModel<Completion> {
        let mut built = self.client.completion(model);
        let anthropic = vendor("anthropic");
        let mapped = self
            .model_facts
            .get(model)
            .and_then(|target| facts_as(anthropic, anthropic, target, model))
            .map(|(facts, ..)| facts);
        if let Some(facts) = mapped.or_else(|| unlisted_facts(anthropic, model)) {
            built.wire = built.wire.with_facts(facts);
        }
        self.caching.apply(built, scope).erase()
    }
}

impl AccountClient for Connected {
    fn completion(&self, model: &str) -> DynModel<Completion> {
        self.model(model, CacheScope::Conversation)
    }

    fn guardian_completion(&self, model: &str) -> DynModel<Completion> {
        self.model(model, CacheScope::StaticPrefix)
    }

    fn catalog_vendor(&self) -> Option<ProviderId> {
        Some(vendor("anthropic"))
    }

    fn facts_notice(&self, model: &str) -> Option<String> {
        let anthropic = vendor("anthropic");
        if let Some(target) = self.model_facts.get(model)
            && let Some((_, from, listed)) = facts_as(anthropic, anthropic, target, model)
        {
            return (!listed).then(|| notice(model, target, &from, None));
        }
        if let Some(derived) = unlisted(anthropic, model) {
            return Some(notice(model, model, &derived.from, None));
        }
        Catalog::builtin()
            .get(anthropic, model)
            .is_none()
            .then(|| no_facts_notice(model, &self.account_name))
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>> {
        async move {
            let list = self.client.list_models().await.map_err(Box::new)?;
            Ok(list.data.into_iter().map(|m| m.id).collect())
        }
        .boxed()
    }
}
