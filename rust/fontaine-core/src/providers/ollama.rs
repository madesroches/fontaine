use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use rig_core::DynModel;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::ollama::{Ollama as Client, OllamaConfig};

use super::{AccountClient, ConnectContext, CredentialPolicy, Provider};
use crate::transport::caused_client;

pub struct Ollama;

impl Provider for Ollama {
    fn name(&self) -> &'static str {
        "ollama"
    }

    fn credential(&self) -> CredentialPolicy {
        CredentialPolicy::Optional
    }

    /// Ollama lists `name:tag`; a bare name means the `latest` tag.
    fn lists(&self, available: &[String], model: &str) -> bool {
        available
            .iter()
            .any(|id| id == model || id.strip_suffix(":latest") == Some(model))
    }

    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String> {
        let mut config = OllamaConfig::new();
        if let Some(url) = ctx.base_url() {
            config = config.with_base_url(url);
        }
        if let Some(key) = ctx.api_key() {
            config = config.with_api_key(key);
        }
        Ok(Arc::new(Connected(config.connect(caused_client()?))))
    }
}

struct Connected(Client);

impl AccountClient for Connected {
    fn completion(&self, model: &str) -> DynModel<Completion> {
        // The native route: `completion()` would target the daemon's `/v1` API.
        self.0.native_completion(model).erase()
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>> {
        async move {
            let list = self.0.list_models().await.map_err(Box::new)?;
            Ok(list.data.into_iter().map(|m| m.id).collect())
        }
        .boxed()
    }
}
