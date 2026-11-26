use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::openai::wire::{OPENROUTER, OpenAiWire};
use rig_core::providers::openai::{OpenAI as Client, OpenAIConfig, Route};
use rig_core::{DynModel, Model};
use serde_json::{Value, json};

use super::cache_wire::CacheWire;
use super::reasoning_parts::ReasoningPartsWire;
use super::warn::Warn;
use rig_core::providers::registry::ProviderId;

use super::derived_facts::unlisted_facts;
use super::{
    AccountClient, CacheScope, ConnectContext, CredentialPolicy, PromptCaching, Provider, vendor,
};
use crate::core::transport::caused_client;
use crate::tools::error_chain::chain;

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

    fn accepts_thinking_off(&self) -> bool {
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
            warn: ctx.warn.clone(),
            thinking_off: ctx.account.thinking_off.unwrap_or(true),
        }))
    }
}

struct Connected {
    client: Client,
    caching: PromptCaching,
    warn: Warn,
    thinking_off: bool,
}

/// On unless the account says `false`: a guardian that reasons by default spends its output budget
/// before answering. An upstream that cannot turn reasoning off refuses the field with a 400 naming
/// it, and the guardian retries without it. The `reasoning` object is gateway-normalized, so one
/// value serves every model.
fn thinking_off(account: Option<bool>) -> Option<Value> {
    account
        .unwrap_or(true)
        .then(|| json!({"reasoning": {"enabled": false}}))
}

impl Connected {
    fn model(&self, model: &str, scope: CacheScope) -> DynModel<Completion> {
        let routed = self.client.completion(model);
        let OpenAiWire::Chat(mut chat) = routed.wire else {
            unreachable!("the client is configured for the Chat route");
        };
        if let Some(facts) = unlisted_facts(vendor("openrouter"), model) {
            chat = chat.with_facts(facts);
        }
        Model::new(
            CacheWire::new(
                ReasoningPartsWire::new(chat),
                model,
                self.caching,
                scope,
                self.warn.clone(),
            ),
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

    fn thinking_off(&self, _model: &str) -> Option<Value> {
        thinking_off(Some(self.thinking_off))
    }

    fn catalog_vendor(&self) -> Option<ProviderId> {
        Some(vendor("openrouter"))
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>> {
        async move {
            let list = self.client.list_models().await.map_err(Box::new)?;
            Ok(list.data.into_iter().map(|m| m.id).collect())
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_off_is_sent_unless_the_account_sets_false() {
        let off = Some(json!({"reasoning": {"enabled": false}}));
        assert_eq!(thinking_off(Some(true)), off);
        assert_eq!(thinking_off(None), off);
        assert_eq!(thinking_off(Some(false)), None);
    }
}
