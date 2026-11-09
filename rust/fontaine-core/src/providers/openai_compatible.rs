use std::sync::Arc;

use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::Auth;

use futures::FutureExt;
use futures::future::BoxFuture;
use rig_core::DynModel;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::openai::OpenAI as Client;

use super::gateway::{GATEWAY, ModelsFetcher};
use super::kimi_quota::KimiQuota;
use super::{AccountClient, ConnectContext, CredentialPolicy, Provider, ServedModel};
use crate::config::{Account, QuotaKind};
use crate::quota::QuotaSource;
use crate::transport::caused_client;
use fontaine_tools::error_chain::chain;

pub struct OpenAiCompatible;

impl Provider for OpenAiCompatible {
    fn name(&self) -> &'static str {
        "openai-compatible"
    }

    fn base_url_policy(&self) -> super::BaseUrlPolicy {
        super::BaseUrlPolicy::Required
    }

    fn credential(&self) -> CredentialPolicy {
        CredentialPolicy::Optional
    }

    fn accepts_quota(&self) -> bool {
        true
    }

    fn validate(&self, account: &Account) -> Result<(), (&'static str, String)> {
        if account.base_url.as_deref().is_none_or(str::is_empty) {
            return Err((
                "base_url",
                "required for kind = \"openai-compatible\"".to_string(),
            ));
        }
        Ok(())
    }

    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String> {
        let mut config = OpenAIConfig::with_key(&GATEWAY, ctx.api_key().unwrap_or_default())
            .with_auth(Auth::OptionalBearer);
        if let Some(url) = ctx.base_url() {
            config = config.with_base_url(url);
        }
        let client = config.connect(caused_client(ctx.network).map_err(|e| chain(&e))?);
        let quota = match (ctx.account.quota, ctx.base_url()) {
            (Some(QuotaKind::Kimi), Some(url)) => Some(Arc::new(
                KimiQuota::new(ctx.network, url, ctx.api_key()).map_err(|e| chain(&e))?,
            )),
            _ => None,
        };
        let models = match ctx.base_url() {
            Some(url) => {
                Some(ModelsFetcher::new(ctx.network, url, ctx.api_key()).map_err(|e| chain(&e))?)
            }
            None => None,
        };
        Ok(Arc::new(Connected {
            client,
            quota,
            models,
        }))
    }
}

struct Connected {
    client: Client,
    quota: Option<Arc<KimiQuota>>,
    /// rig's model listing keeps only ids, so the fuller entry is fetched separately.
    models: Option<ModelsFetcher>,
}

impl AccountClient for Connected {
    fn completion(&self, model: &str) -> DynModel<Completion> {
        self.client.completion(model).erase()
    }

    fn quota_source(&self) -> Option<Arc<dyn QuotaSource>> {
        self.quota
            .clone()
            .map(|quota| quota as Arc<dyn QuotaSource>)
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>> {
        async move {
            let list = self.client.list_models().await.map_err(Box::new)?;
            Ok(list.data.into_iter().map(|m| m.id).collect())
        }
        .boxed()
    }

    fn served_model(&self, model: &str) -> BoxFuture<'_, Result<Option<ServedModel>, String>> {
        let model = model.to_string();
        async move {
            match &self.models {
                Some(models) => models.served(&model).await,
                None => Ok(None),
            }
        }
        .boxed()
    }
}
