//! The shared gateway dialects and `/models` entry lookup of the Chat Completions kinds.

use rig_core::providers::openai::wire::Dialect;

use super::ServedModel;
use super::kimi_quota::REQUEST_TIMEOUT;
use crate::transport::reqwest_client;
use fontaine_tools::error_chain::chain;

/// The one gateway dialect every `openai-compatible` account speaks. Its name is the vendor
/// rig prices by, so it is the kind name and never an account name: such endpoints are unpriced.
pub(crate) const GATEWAY: Dialect = Dialect::gateway("openai-compatible", "", "");

/// The dialect a `kimi` account speaks: the kind name keeps its requests distinct from a real
/// `openai-compatible` one in provider fields, OTLP attributes and metric labels.
pub(crate) const KIMI_DIALECT: Dialect = Dialect::gateway("kimi", "", "");

/// rig's model listing keeps only ids, so the fuller entry is fetched separately.
pub(crate) struct ModelsFetcher {
    client: reqwest::Client,
    url: String,
    key: Option<String>,
}

impl ModelsFetcher {
    pub(crate) fn new(
        network: &fontaine_tools::tls::LoadedNetwork,
        base_url: &str,
        key: Option<String>,
    ) -> Result<Self, crate::RuntimeError> {
        Ok(ModelsFetcher {
            client: reqwest_client(network)?,
            url: format!("{}/models", base_url.trim_end_matches('/')),
            key,
        })
    }

    /// `Err` carries why the listing could not be read; `Ok(None)` is a listing without the model.
    pub(crate) async fn served(&self, model: &str) -> Result<Option<ServedModel>, String> {
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
pub(crate) fn parse_served(body: &str, model: &str) -> Option<ServedModel> {
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
