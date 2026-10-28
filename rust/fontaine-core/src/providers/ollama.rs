use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use rig_core::catalog::{Catalog, ModelFacts};
use rig_core::completion::{CompletionRequest, Reasoning};
use rig_core::error::{EncodeError, ProviderError};
use rig_core::operation::Completion;
use rig_core::providers::ollama::extension::OllamaOptions;
use rig_core::providers::ollama::{Chat, Ollama as Client, OllamaConfig};
use rig_core::providers::registry::ProviderId;
use rig_core::wire::{Descriptor, Encoded, Mode, Wire};
use rig_core::{DynModel, Model};

use super::{AccountClient, ConnectContext, CredentialPolicy, Provider};
use crate::transport::{caused_client, reqwest_client};
use fontaine_guardian::{OutputMode, OutputProbe, ProbeOutcome};
use fontaine_tools::error_chain::chain;

const DEFAULT_ENDPOINT: &str = "http://localhost:11434";

pub struct Ollama;

impl Provider for Ollama {
    fn name(&self) -> &'static str {
        "ollama"
    }

    fn base_url_policy(&self) -> super::BaseUrlPolicy {
        super::BaseUrlPolicy::Optional
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

    fn default_endpoint(&self) -> Option<&'static str> {
        Some(DEFAULT_ENDPOINT)
    }

    fn accepts_model_options(&self) -> bool {
        true
    }

    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String> {
        let mut config = OllamaConfig::new();
        if let Some(url) = ctx.base_url() {
            config = config.with_base_url(url);
        }
        if let Some(key) = ctx.api_key() {
            config = config.with_api_key(key);
        }
        let base_url = ctx
            .base_url()
            .unwrap_or(DEFAULT_ENDPOINT)
            .trim_end_matches('/')
            .to_owned();
        Ok(Arc::new(Connected {
            client: config.connect(caused_client(ctx.network).map_err(|e| chain(&e))?),
            show: Show {
                http: reqwest_client(ctx.network).map_err(|e| chain(&e))?,
                base_url,
                api_key: ctx.api_key(),
            },
            context_tokens: ctx.account.ollama_context_tokens(),
            think: ctx.account.think,
        }))
    }
}

struct Connected {
    client: Client,
    show: Show,
    context_tokens: u32,
    think: Option<bool>,
}

/// What `/api/show` needs, which rig's client does not expose.
#[derive(Clone)]
struct Show {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

impl std::fmt::Debug for Show {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Show")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

/// Asks the daemon what a model can do, without loading it.
#[derive(Debug)]
struct CapabilityProbe {
    show: Show,
    model: String,
}

impl OutputProbe for CapabilityProbe {
    fn probe(&self) -> BoxFuture<'_, ProbeOutcome> {
        async move {
            match self.capabilities().await {
                Ok(capabilities) => outcome_for(capabilities.as_deref()),
                Err(e) => {
                    tracing::debug!(error = %chain(&e), "the Ollama capability probe failed");
                    ProbeOutcome::Unreachable
                }
            }
        }
        .boxed()
    }
}

#[derive(serde::Deserialize)]
struct ShowReply {
    capabilities: Option<Vec<String>>,
}

impl CapabilityProbe {
    /// `None` when the daemon is too old to report them.
    async fn capabilities(&self) -> Result<Option<Vec<String>>, reqwest::Error> {
        let mut request = self
            .show
            .http
            .post(format!("{}/api/show", self.show.base_url))
            .json(&serde_json::json!({ "model": self.model }));
        if let Some(key) = &self.show.api_key {
            request = request.bearer_auth(key);
        }
        let reply: ShowReply = request
            .send()
            .await
            .map_err(reqwest::Error::without_url)?
            .error_for_status()
            .map_err(reqwest::Error::without_url)?
            .json()
            .await
            .map_err(reqwest::Error::without_url)?;
        Ok(reply.capabilities)
    }
}

/// A model with tool support takes the benchmarked tool path. One that can generate but has no
/// tool template takes the schema-constrained path. A daemon that reports nothing gives no
/// evidence to swap the path on, so it stays on the tool path and fails loudly if it must.
fn outcome_for(capabilities: Option<&[String]>) -> ProbeOutcome {
    let has = |name: &str| capabilities.is_some_and(|c| c.iter().any(|c| c == name));
    match capabilities {
        None | Some([]) => ProbeOutcome::Mode(OutputMode::Tool),
        _ if has("tools") => ProbeOutcome::Mode(OutputMode::Tool),
        _ if has("completion") => ProbeOutcome::Mode(OutputMode::Schema),
        Some(listed) => ProbeOutcome::Incapable(format!(
            "cannot generate text (capabilities: {})",
            listed.join(", ")
        )),
    }
}

/// The native chat wire with every request pinned to the account's window. Ollama loads a model
/// with its own default window (4096) otherwise and silently cuts an over-long prompt from the
/// front, so each request names `num_ctx` and asks the daemon to refuse rather than truncate.
#[derive(Clone)]
struct Tuned {
    chat: Chat,
    context_tokens: u32,
    think: Option<bool>,
}

impl Tuned {
    fn tune(&self, mut request: CompletionRequest) -> CompletionRequest {
        request = request.provider_option(OllamaOptions::default().num_ctx(self.context_tokens));
        match &mut request.additional_params {
            Some(serde_json::Value::Object(params)) => {
                params.insert("truncate".to_owned(), false.into());
            }
            None => request.additional_params = Some(serde_json::json!({"truncate": false})),
            // Not an object: rig refuses the request itself.
            Some(_) => {}
        }
        if self.think == Some(false) && request.options.reasoning.is_none() {
            request.options.reasoning = Some(Reasoning::Off);
        }
        request
    }
}

impl Wire for Tuned {
    type Op = Completion;
    type Payload = <Chat as Wire>::Payload;
    type Frame = <Chat as Wire>::Frame;
    type Event = <Chat as Wire>::Event;
    type Decoder<'id> = <Chat as Wire>::Decoder<'id>;
    type Reassembler = <Chat as Wire>::Reassembler;

    fn describe(&self) -> Descriptor<'_> {
        self.chat.describe()
    }

    fn encode(&self, request: CompletionRequest, mode: Mode) -> Result<Encoded, EncodeError> {
        self.chat.encode(self.tune(request), mode)
    }

    fn decoder<'id>(&self) -> Self::Decoder<'id> {
        self.chat.decoder()
    }

    fn reassembler(&self) -> Self::Reassembler {
        self.chat.reassembler()
    }
}

impl AccountClient for Connected {
    fn completion(&self, model: &str) -> DynModel<Completion> {
        // The native route: `completion()` would target the daemon's `/v1` API.
        let native = self.client.native_completion(model);
        let mut chat = native.wire;
        // Threshold compaction reads the window from the spec, so a model the catalog lists is
        // bound to the window the daemon actually loads. An unlisted model stays unbound: a bare
        // spec would also override rig's per-vendor tool and image rules.
        if let Some(entry) =
            ProviderId::catalog("ollama").and_then(|id| Catalog::builtin().get(id, model))
        {
            let mut spec = entry.spec.clone();
            spec.context_window = Some(self.context_tokens);
            spec.id = model.to_owned();
            chat = chat.with_facts(ModelFacts::new(spec));
        }
        Model::new(
            Tuned {
                chat,
                context_tokens: self.context_tokens,
                think: self.think,
            },
            native.transport,
        )
        .erase()
    }

    fn output_probe(&self, model: &str) -> Option<Arc<dyn OutputProbe>> {
        Some(Arc::new(CapabilityProbe {
            show: self.show.clone(),
            model: model.to_owned(),
        }))
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

    fn caps(names: &[&str]) -> Option<Vec<String>> {
        Some(names.iter().map(|n| n.to_string()).collect())
    }

    #[test]
    fn capabilities_choose_the_transport() {
        let outcome = |names: Option<Vec<String>>| outcome_for(names.as_deref());
        assert_eq!(
            outcome(caps(&["completion", "tools"])),
            ProbeOutcome::Mode(OutputMode::Tool)
        );
        assert_eq!(
            outcome(caps(&["completion", "vision"])),
            ProbeOutcome::Mode(OutputMode::Schema)
        );
        assert_eq!(
            outcome(caps(&["embedding"])),
            ProbeOutcome::Incapable("cannot generate text (capabilities: embedding)".into())
        );
    }

    #[test]
    fn a_daemon_that_reports_nothing_keeps_the_tool_path() {
        assert_eq!(outcome_for(None), ProbeOutcome::Mode(OutputMode::Tool));
        assert_eq!(outcome_for(Some(&[])), ProbeOutcome::Mode(OutputMode::Tool));
    }
}
