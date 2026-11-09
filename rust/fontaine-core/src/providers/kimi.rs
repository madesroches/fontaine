//! Kimi for Coding: the flat-rate subscription, `https://api.kimi.com/coding/v1`.
//!
//! The moonshot.ai pay-as-you-go API is a different product and stays on `openai-compatible`.

use std::sync::Arc;

use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::Auth;

use futures::FutureExt;
use futures::future::BoxFuture;
use rig_core::DynModel;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::openai::OpenAI as Client;

use super::gateway::{KIMI_DIALECT, ModelsFetcher};
use super::kimi_quota::KimiQuota;
use super::{AccountClient, Billing, ConnectContext, CredentialPolicy, Provider, ServedModel};
use crate::quota::QuotaSource;
use crate::transport::caused_client;
use fontaine_tools::error_chain::chain;

const ENDPOINT: &str = "https://api.kimi.com/coding/v1";

pub struct Kimi;

impl Provider for Kimi {
    fn name(&self) -> &'static str {
        "kimi"
    }

    fn credential(&self) -> CredentialPolicy {
        CredentialPolicy::Optional
    }

    fn billing(&self) -> Billing {
        Billing::Unpriced
    }

    fn default_endpoint(&self) -> Option<&'static str> {
        Some(ENDPOINT)
    }

    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String> {
        let endpoint = ctx
            .base_url()
            .or_else(|| self.default_endpoint())
            .expect("a Fixed kind always resolves an endpoint");
        let config = OpenAIConfig::with_key(&KIMI_DIALECT, ctx.api_key().unwrap_or_default())
            .with_auth(Auth::OptionalBearer)
            .with_base_url(endpoint);
        let client = config.connect(caused_client(ctx.network).map_err(|e| chain(&e))?);
        let quota =
            Arc::new(KimiQuota::new(ctx.network, endpoint, ctx.api_key()).map_err(|e| chain(&e))?);
        let models =
            ModelsFetcher::new(ctx.network, endpoint, ctx.api_key()).map_err(|e| chain(&e))?;
        Ok(Arc::new(Connected {
            client,
            quota,
            models,
        }))
    }
}

struct Connected {
    client: Client,
    quota: Arc<KimiQuota>,
    models: ModelsFetcher,
}

impl AccountClient for Connected {
    fn completion(&self, model: &str) -> DynModel<Completion> {
        self.client.completion(model).erase()
    }

    fn quota_source(&self) -> Option<Arc<dyn QuotaSource>> {
        Some(self.quota.clone() as Arc<dyn QuotaSource>)
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
        async move { self.models.served(&model).await }.boxed()
    }
}

#[cfg(test)]
mod tests {
    use fontaine_tools::tls::LoadedNetwork;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    use super::*;
    use crate::config::Account;
    use crate::credentials::StoredCredential;
    use crate::providers::ConnectContext;

    const USAGES: &str = r#"{
        "usages": {
            "limit_5h": {"used_ratio": 0.82, "reset_time": "2026-11-02T15:00:00Z"},
            "limit_month_total": {"used_ratio": 0.16, "reset_time": "2026-12-01T00:00:00Z"}
        }
    }"#;

    fn account(base_url: Option<String>) -> Account {
        Account::new(crate::config::AccountKind::named("kimi"), base_url)
    }

    fn connect(account: &Account, key: Option<&str>) -> Arc<dyn AccountClient> {
        let credential = key.map(|k| StoredCredential::ApiKey(k.into()));
        Kimi.connect(ConnectContext {
            account_name: "k",
            account,
            credential,
            data_dir: std::path::Path::new("."),
            session_id: None,
            network: &LoadedNetwork::default(),
            env: std::sync::Arc::new(|_| None),
            warn: crate::providers::warn::Warn::process(),
        })
        .expect("the client connects")
    }

    #[tokio::test]
    async fn a_keyless_account_lists_models_with_no_authorization_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"data": [{"id": "kimi-for-coding"}]}"#),
            )
            .mount(&server)
            .await;
        let client = connect(&account(Some(server.uri())), None);
        assert_eq!(client.list().await.unwrap(), ["kimi-for-coding"]);
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests
                .iter()
                .all(|r: &Request| !r.headers.contains_key("authorization"))
        );
    }

    #[tokio::test]
    async fn the_key_is_sent_to_models_and_usages_when_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("authorization", "Bearer sk-x"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"data": [{"id": "kimi-for-coding"}]}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/usages"))
            .and(header("authorization", "Bearer sk-x"))
            .respond_with(ResponseTemplate::new(200).set_body_string(USAGES))
            .mount(&server)
            .await;
        let client = connect(&account(Some(server.uri())), Some("sk-x"));
        assert_eq!(client.list().await.unwrap(), ["kimi-for-coding"]);
        let quota = client
            .quota_source()
            .expect("the quota is always attached")
            .fetch()
            .await
            .expect("the quota replies");
        assert_eq!(quota.windows.len(), 2);
    }

    #[tokio::test]
    async fn a_keyless_account_fetches_quota_from_usages_with_no_authorization_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/usages"))
            .respond_with(ResponseTemplate::new(200).set_body_string(USAGES))
            .mount(&server)
            .await;
        let client = connect(&account(Some(server.uri())), None);
        let quota = client
            .quota_source()
            .expect("the quota is always attached")
            .fetch()
            .await
            .expect("the quota replies");
        assert_eq!(quota.windows.len(), 2);
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests
                .iter()
                .all(|r: &Request| !r.headers.contains_key("authorization"))
        );
    }

    #[tokio::test]
    async fn without_a_base_url_the_default_endpoint_applies_and_quota_is_attached() {
        let client = connect(&account(None), None);
        assert!(client.quota_source().is_some());
    }
}
