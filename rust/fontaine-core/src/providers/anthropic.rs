use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use rig_core::DynModel;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::anthropic::wire::Auth;
use rig_core::providers::anthropic::{Anthropic as Client, AnthropicConfig};

use rig_core::providers::registry::ProviderId;

use super::derived_facts::unlisted_facts;
use super::{
    AccountClient, CacheScope, ConnectContext, CredentialPolicy, PromptCaching, Provider, vendor,
};
use crate::config::AnthropicAuth;
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
        }))
    }
}

pub(super) struct Connected {
    client: Client,
    caching: PromptCaching,
}

impl Connected {
    fn model(&self, model: &str, scope: CacheScope) -> DynModel<Completion> {
        let mut built = self.client.completion(model);
        if let Some(facts) = unlisted_facts(vendor("anthropic"), model) {
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

    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>> {
        async move {
            let list = self.client.list_models().await.map_err(Box::new)?;
            Ok(list.data.into_iter().map(|m| m.id).collect())
        }
        .boxed()
    }
}
