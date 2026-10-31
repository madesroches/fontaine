use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::openai::wire::{OPENROUTER, OpenAiWire};
use rig_core::providers::openai::{OpenAI as Client, OpenAIConfig, Route};
use rig_core::{DynModel, Model};

use super::cache_wire::CacheWire;
use super::{AccountClient, CacheScope, ConnectContext, CredentialPolicy, PromptCaching, Provider};
use crate::transport::caused_client;
use fontaine_tools::error_chain::chain;

pub struct OpenRouter;

impl Provider for OpenRouter {
    fn name(&self) -> &'static str {
        "openrouter"
    }

    fn credential(&self) -> CredentialPolicy {
        CredentialPolicy::Required
    }

    fn default_endpoint(&self) -> Option<&'static str> {
        Some("https://openrouter.ai/api/v1")
    }

    fn accepts_prompt_caching(&self) -> bool {
        true
    }

    fn default_prompt_caching(&self) -> bool {
        true
    }

    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String> {
        // Chat only: the marker rewrite lives on that wire, and every upstream speaks it.
        let mut config = OpenAIConfig::with_key(&OPENROUTER, ctx.api_key().unwrap_or_default())
            .with_route(Route::Chat);
        if let Some(url) = ctx.base_url() {
            config = config.with_base_url(url);
        }
        Ok(Arc::new(Connected {
            client: config.connect(caused_client(ctx.network).map_err(|e| chain(&e))?),
            caching: PromptCaching::of(ctx.account),
        }))
    }
}

struct Connected {
    client: Client,
    caching: PromptCaching,
}

impl Connected {
    fn model(&self, model: &str, scope: CacheScope) -> DynModel<Completion> {
        let routed = self.client.completion(model);
        let OpenAiWire::Chat(chat) = routed.wire else {
            unreachable!("the client is configured for the Chat route");
        };
        Model::new(
            CacheWire::new(chat, model, self.caching, scope),
            routed.transport,
        )
        .erase()
    }
}

impl AccountClient for Connected {
    fn completion(&self, model: &str) -> DynModel<Completion> {
        self.model(model, CacheScope::Conversation)
    }

    fn guardian_completion(&self, model: &str) -> DynModel<Completion> {
        self.model(model, CacheScope::StaticPrefix)
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>> {
        async move {
            let list = self.client.list_models().await.map_err(Box::new)?;
            Ok(list.data.into_iter().map(|m| m.id).collect())
        }
        .boxed()
    }
}
