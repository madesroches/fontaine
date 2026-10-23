use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use rig_core::DynModel;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::gemini::{Gemini as Client, GeminiConfig};

use super::{AccountClient, ConnectContext, CredentialPolicy, Provider};
use crate::transport::caused_client;

pub struct Gemini;

impl Provider for Gemini {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn credential(&self) -> CredentialPolicy {
        CredentialPolicy::Required
    }

    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String> {
        let mut config = GeminiConfig::new(ctx.api_key().unwrap_or_default());
        if let Some(url) = ctx.base_url() {
            config = config.with_base_url(url);
        }
        Ok(Arc::new(Connected(config.connect(caused_client()?))))
    }
}

pub(super) struct Connected(pub(super) Client);

impl AccountClient for Connected {
    fn completion(&self, model: &str) -> DynModel<Completion> {
        self.0.completion(model).erase()
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>> {
        async move {
            let list = self.0.list_models().await.map_err(Box::new)?;
            Ok(list.data.into_iter().map(|m| m.id).collect())
        }
        .boxed()
    }
}
