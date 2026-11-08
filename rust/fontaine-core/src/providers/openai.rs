use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use rig_core::DynModel;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::openai::{OpenAI as Client, OpenAIConfig};

use rig_core::providers::registry::ProviderId;

use super::derived_facts::unlisted_facts;
use super::{AccountClient, ConnectContext, CredentialPolicy, Provider, vendor};
use crate::transport::caused_client;
use fontaine_tools::error_chain::chain;

pub struct OpenAi;

impl Provider for OpenAi {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn credential(&self) -> CredentialPolicy {
        CredentialPolicy::Required
    }

    fn default_endpoint(&self) -> Option<&'static str> {
        Some("https://api.openai.com/v1")
    }

    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String> {
        let mut config = OpenAIConfig::new(ctx.api_key().unwrap_or_default());
        if let Some(url) = ctx.base_url() {
            config = config.with_base_url(url);
        }
        Ok(Arc::new(Connected(config.connect(
            caused_client(ctx.network).map_err(|e| chain(&e))?,
        ))))
    }
}

struct Connected(Client);

impl AccountClient for Connected {
    fn completion(&self, model: &str) -> DynModel<Completion> {
        let mut built = self.0.completion(model);
        if let Some(facts) = unlisted_facts(vendor("openai"), model) {
            built.wire = built.wire.with_facts(facts);
        }
        built.erase()
    }

    fn catalog_vendor(&self) -> Option<ProviderId> {
        Some(vendor("openai"))
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>> {
        async move {
            let list = self.0.list_models().await.map_err(Box::new)?;
            Ok(list.data.into_iter().map(|m| m.id).collect())
        }
        .boxed()
    }
}
