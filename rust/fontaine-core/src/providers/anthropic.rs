use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use rig_core::DynModel;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::anthropic::{Anthropic as Client, AnthropicConfig};

use super::{AccountClient, CacheScope, ConnectContext, CredentialPolicy, PromptCaching, Provider};
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

    fn accepts_prompt_caching(&self) -> bool {
        true
    }

    fn default_prompt_caching(&self) -> bool {
        true
    }

    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String> {
        let mut config = AnthropicConfig::new(ctx.api_key().unwrap_or_default());
        if let Some(url) = ctx.base_url() {
            config = config.with_base_url(url);
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

impl AccountClient for Connected {
    fn completion(&self, model: &str) -> DynModel<Completion> {
        self.caching
            .apply(self.client.completion(model), CacheScope::Conversation)
            .erase()
    }

    fn guardian_completion(&self, model: &str) -> DynModel<Completion> {
        self.caching
            .apply(self.client.completion(model), CacheScope::StaticPrefix)
            .erase()
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>> {
        async move {
            let list = self.client.list_models().await.map_err(Box::new)?;
            Ok(list.data.into_iter().map(|m| m.id).collect())
        }
        .boxed()
    }
}
