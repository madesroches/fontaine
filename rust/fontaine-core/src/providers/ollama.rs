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
use crate::transport::caused_client;

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
        Some("http://localhost:11434")
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
        Ok(Arc::new(Connected {
            client: config.connect(caused_client()?),
            context_tokens: ctx.account.ollama_context_tokens(),
            think: ctx.account.think,
        }))
    }
}

struct Connected {
    client: Client,
    context_tokens: u32,
    think: Option<bool>,
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

    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>> {
        async move {
            let list = self.client.list_models().await.map_err(Box::new)?;
            Ok(list.data.into_iter().map(|m| m.id).collect())
        }
        .boxed()
    }
}
