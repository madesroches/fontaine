use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use rig_core::completion::ProviderOptions;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::anthropic::extension::{
    AnthropicOptions, CONTEXT_MANAGEMENT_BETA, ContextManagement,
};
use rig_core::providers::anthropic::wire::Auth;
use rig_core::providers::anthropic::{Anthropic as Client, AnthropicConfig, Messages};
use rig_core::{DynModel, Model};

use rig_core::providers::registry::ProviderId;

use std::collections::BTreeMap;

use rig_core::catalog::Catalog;

use super::context_editing;
use super::defer_wire::DeferWire;
use super::derived_facts::{Price, facts_as, no_facts_notice, notice, unlisted, unlisted_facts};
use super::warn::Warn;
use super::{
    AccountClient, CacheScope, ConnectContext, CredentialPolicy, PromptCaching, Provider, vendor,
};
use crate::core::config::{Account, AnthropicAuth};
use crate::core::transport::caused_client;
use crate::tools::error_chain::chain;
use crate::tools::mcp::Deferral;

pub struct Anthropic;

impl Provider for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn credential(&self) -> CredentialPolicy {
        CredentialPolicy::Required
    }

    fn default_endpoint(&self) -> Option<&'static str> {
        Some("https://api.anthropic.com")
    }

    // The version every Anthropic hosting accepts; Azure supports only this one.
    fn native_web_search(&self) -> Option<serde_json::Value> {
        Some(super::anthropic_web_search())
    }

    fn accepts_prompt_caching(&self) -> bool {
        true
    }

    fn default_prompt_caching(&self) -> bool {
        true
    }

    fn accepts_context_editing(&self) -> bool {
        true
    }

    fn native_deferral(&self) -> bool {
        true
    }

    fn accepts_auth(&self) -> bool {
        true
    }

    fn accepts_model_facts(&self) -> bool {
        true
    }

    fn validate(&self, account: &Account) -> Result<(), (&'static str, String)> {
        let anthropic = vendor("anthropic");
        for (served, target) in account.model_facts.iter().flatten() {
            if Catalog::builtin().get(anthropic, served).is_some() {
                return Err((
                    "model_facts",
                    format!(
                        "`{served}` is a model the catalog lists, which keeps its own facts; \
                         map only a name the host invented"
                    ),
                ));
            }
            if facts_as(anthropic, anthropic, target, served, Price::Strip).is_none() {
                return Err((
                    "model_facts",
                    format!("`{served}` maps to `{target}`, which is not a catalog Claude model"),
                ));
            }
        }
        Ok(())
    }

    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String> {
        let mut config = AnthropicConfig::new(ctx.api_key().unwrap_or_default());
        if let Some(url) = ctx.base_url() {
            config = config.with_base_url(url);
        }
        if ctx.account.auth == Some(AnthropicAuth::Bearer) {
            config.dialect.quirks.auth = Auth::OptionalBearer;
        }
        let edit = ctx
            .account
            .context_editing
            .as_ref()
            .filter(|editing| editing.enabled)
            .map(context_editing::edit_json);
        // rig sends the body field but not the header it needs; the header rides on every
        // completion of the account and is inert without the field.
        if edit.is_some() {
            config = config.with_beta(CONTEXT_MANAGEMENT_BETA);
        }
        Ok(Arc::new(Connected {
            context_edit: edit,
            client: config.connect(caused_client(ctx.network).map_err(|e| chain(&e))?),
            caching: PromptCaching::of(ctx.account),
            account_name: ctx.account_name.to_string(),
            model_facts: ctx.account.model_facts.clone().unwrap_or_default(),
            warn: ctx.warn.clone(),
        }))
    }
}

pub(super) struct Connected {
    client: Client,
    caching: PromptCaching,
    context_edit: Option<serde_json::Value>,
    account_name: String,
    model_facts: BTreeMap<String, String>,
    warn: Warn,
}

/// Whether `model` takes `defer_loading` tools and `tool_reference` results. Claude 3 and the
/// first Opus 4, Opus 4.1 and Sonnet 4 models do not; every other id is assumed to, and a host
/// that rejects the fields anyway is caught by the request retry. An explicit list because the
/// catalog carries no release date for those entries.
pub(crate) fn model_defers_natively(model: &str) -> bool {
    if model.starts_with("claude-3") {
        return false;
    }
    let opus = model.strip_prefix("claude-opus-4");
    let sonnet = model.strip_prefix("claude-sonnet-4");
    let Some(rest) = opus.or(sonnet) else {
        return true;
    };
    let Some(segment) = rest.strip_prefix('-') else {
        // The bare family name is the first release.
        return !rest.is_empty();
    };
    let dated = segment.len() == 8 && segment.bytes().all(|b| b.is_ascii_digit());
    let first_minor = |minor: &str| segment == minor || segment.starts_with(&format!("{minor}-"));
    !(dated || first_minor("0") || (opus.is_some() && first_minor("1")))
}

impl Connected {
    fn built(&self, model: &str, scope: CacheScope) -> Model<Messages> {
        let mut built = self.client.completion(model);
        let anthropic = vendor("anthropic");
        let mapped = self
            .model_facts
            .get(model)
            .and_then(|target| facts_as(anthropic, anthropic, target, model, Price::Strip))
            .map(|(facts, ..)| facts);
        if let Some(facts) = mapped.or_else(|| unlisted_facts(anthropic, model)) {
            built.wire = built.wire.with_facts(facts);
        }
        self.caching.apply(built, scope)
    }

    fn model(&self, model: &str, scope: CacheScope) -> DynModel<Completion> {
        self.built(model, scope).erase()
    }
}

impl AccountClient for Connected {
    fn completion(&self, model: &str) -> DynModel<Completion> {
        self.model(model, CacheScope::Conversation)
    }

    fn guardian_completion(&self, model: &str) -> DynModel<Completion> {
        self.model(model, CacheScope::StaticPrefix)
    }

    fn deferring_completion(
        &self,
        model: &str,
        deferral: &Deferral,
        native: bool,
    ) -> DynModel<Completion> {
        let built = self.built(model, CacheScope::Conversation);
        Model::new(
            DeferWire::new(built.wire, deferral.clone(), native, self.warn.clone()),
            built.transport,
        )
        .erase()
    }

    fn provider_options(&self) -> Option<ProviderOptions> {
        let edit = self.context_edit.clone()?;
        Some(ProviderOptions::new().set(
            AnthropicOptions::default().context_management(ContextManagement::new(vec![edit])),
        ))
    }

    fn catalog_vendor(&self) -> Option<ProviderId> {
        Some(vendor("anthropic"))
    }

    fn facts_notice(&self, model: &str) -> Option<String> {
        let anthropic = vendor("anthropic");
        if let Some(target) = self.model_facts.get(model)
            && let Some((_, from, listed)) =
                facts_as(anthropic, anthropic, target, model, Price::Strip)
        {
            return (!listed).then(|| notice(model, target, &from, None));
        }
        if let Some(derived) = unlisted(anthropic, model) {
            return Some(notice(model, model, &derived.from, None));
        }
        Catalog::builtin()
            .get(anthropic, model)
            .is_none()
            .then(|| no_facts_notice(model, &self.account_name))
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
    use super::model_defers_natively;

    #[test]
    fn models_without_native_deferral_are_named_in_alias_and_dated_form() {
        for model in [
            "claude-3-5-haiku-latest",
            "claude-3-7-sonnet-20250219",
            "claude-opus-4-0",
            "claude-opus-4-1",
            "claude-opus-4-1-20250805",
            "claude-opus-4-20250514",
            "claude-sonnet-4-0",
            "claude-sonnet-4-20250514",
        ] {
            assert!(!model_defers_natively(model), "{model}");
        }
    }

    #[test]
    fn every_other_id_is_assumed_to_defer_natively() {
        for model in [
            "claude-opus-4-5",
            "claude-opus-4-5-20251101",
            "claude-opus-4-10",
            "claude-sonnet-4-5",
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5",
            "claude-sonnet-5-5",
            "some-gateway-model",
        ] {
            assert!(model_defers_natively(model), "{model}");
        }
    }
}
