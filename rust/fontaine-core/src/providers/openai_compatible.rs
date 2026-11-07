use std::sync::Arc;

use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::{Auth, Dialect};

use futures::FutureExt;
use futures::future::BoxFuture;
use rig_core::DynModel;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::openai::OpenAI as Client;

use super::kimi_quota::{KimiQuota, REQUEST_TIMEOUT};
use super::{AccountClient, ConnectContext, CredentialPolicy, Provider, ServedModel};
use crate::config::{Account, QuotaKind};
use crate::quota::QuotaSource;
use crate::transport::{caused_client, reqwest_client};
use fontaine_tools::error_chain::chain;

/// The one gateway dialect every `openai-compatible` account speaks. Its name is the vendor
/// rig prices by, so it is the kind name and never an account name: such endpoints are unpriced.
const GATEWAY: Dialect = Dialect::gateway("openai-compatible", "", "");

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
            Some(url) => Some(ModelsFetcher {
                client: reqwest_client(ctx.network).map_err(|e| chain(&e))?,
                url: format!("{}/models", url.trim_end_matches('/')),
                key: ctx.api_key(),
            }),
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

struct ModelsFetcher {
    client: reqwest::Client,
    url: String,
    key: Option<String>,
}

impl ModelsFetcher {
    /// `Err` carries why the listing could not be read; `Ok(None)` is a listing without the model.
    async fn served(&self, model: &str) -> Result<Option<ServedModel>, String> {
        let mut request = self.client.get(&self.url).timeout(REQUEST_TIMEOUT);
        if let Some(key) = &self.key {
            request = request.bearer_auth(key);
        }
        let response = request.send().await.map_err(|e| {
            format!(
                "could not fetch the model list: {}",
                chain(&e.without_url())
            )
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("the model list replied {status}"));
        }
        let body = response
            .text()
            .await
            .map_err(|e| format!("could not read the model list: {}", chain(&e.without_url())))?;
        Ok(parse_served(&body, model))
    }
}

/// The entry for `model` in a `{"data": [...]}` or bare-array `/models` reply.
fn parse_served(body: &str, model: &str) -> Option<ServedModel> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let entries = value.get("data").unwrap_or(&value).as_array()?;
    let entry = entries
        .iter()
        .find(|e| e.get("id").and_then(|v| v.as_str()) == Some(model))?;
    let text = |key: &str| entry.get(key).and_then(|v| v.as_str()).map(str::to_string);
    Some(ServedModel {
        id: model.to_string(),
        display_name: text("display_name"),
        context_length: entry
            .get("context_length")
            .or_else(|| entry.pointer("/limit/context"))
            .and_then(|v| v.as_u64()),
    })
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

#[cfg(test)]
mod tests {
    use fontaine_tools::tls::LoadedNetwork;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn fetcher(server: &MockServer, key: Option<&str>) -> ModelsFetcher {
        ModelsFetcher {
            client: reqwest_client(&LoadedNetwork::default()).expect("the client builds"),
            url: format!("{}/coding/v1/models", server.uri()),
            key: key.map(str::to_string),
        }
    }

    async fn serve(body: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/coding/v1/models"))
            .respond_with(body)
            .mount(&server)
            .await;
        server
    }

    fn kimi() -> ServedModel {
        ServedModel {
            id: "kimi-for-coding".into(),
            display_name: Some("K2.8 Preview".into()),
            context_length: Some(1_048_576),
        }
    }

    const ENTRY: &str =
        r#"{"id": "kimi-for-coding", "display_name": "K2.8 Preview", "context_length": 1048576}"#;

    #[tokio::test]
    async fn a_wrapped_reply_yields_the_entry() {
        let body = format!(r#"{{"data": [{{"id": "other"}}, {ENTRY}]}}"#);
        let server = serve(ResponseTemplate::new(200).set_body_string(body)).await;
        let served = fetcher(&server, None).served("kimi-for-coding").await;
        assert_eq!(served, Ok(Some(kimi())));
    }

    #[tokio::test]
    async fn a_bare_array_yields_the_entry_and_the_key_is_sent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/coding/v1/models"))
            .and(header("authorization", "Bearer sk-x"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!("[{ENTRY}]")))
            .mount(&server)
            .await;
        let served = fetcher(&server, Some("sk-x"))
            .served("kimi-for-coding")
            .await;
        assert_eq!(served, Ok(Some(kimi())));
    }

    #[tokio::test]
    async fn a_missing_entry_yields_none() {
        let server =
            serve(ResponseTemplate::new(200).set_body_string(r#"{"data": [{"id": "other"}]}"#))
                .await;
        assert_eq!(
            fetcher(&server, None).served("kimi-for-coding").await,
            Ok(None)
        );
    }

    #[tokio::test]
    async fn an_http_error_yields_none() {
        let server = serve(ResponseTemplate::new(500)).await;
        let reason = fetcher(&server, None)
            .served("kimi-for-coding")
            .await
            .expect_err("a 500 is a failure");
        assert!(reason.contains("500"), "{reason}");
    }

    #[tokio::test]
    async fn missing_optional_fields_stay_absent_and_limit_context_is_a_fallback() {
        let bare = serve(
            ResponseTemplate::new(200).set_body_string(r#"{"data": [{"id": "kimi-for-coding"}]}"#),
        )
        .await;
        assert_eq!(
            fetcher(&bare, None).served("kimi-for-coding").await,
            Ok(Some(ServedModel {
                id: "kimi-for-coding".into(),
                display_name: None,
                context_length: None,
            }))
        );
        let limited = serve(ResponseTemplate::new(200).set_body_string(
            r#"{"data": [{"id": "kimi-for-coding", "limit": {"context": 262144}}]}"#,
        ))
        .await;
        assert_eq!(
            fetcher(&limited, None)
                .served("kimi-for-coding")
                .await
                .ok()
                .flatten()
                .and_then(|s| s.context_length),
            Some(262_144)
        );
    }
}
