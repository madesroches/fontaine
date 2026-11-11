//! Databricks Unity Gateway: one workspace and one token serving models on four wires. Claude
//! goes to the Anthropic Messages route (with prompt caching), GPT to the OpenAI Responses
//! route, Gemini to the native Gemini route, and every other model, including custom model
//! services, to the provider-neutral MLflow Chat Completions route. Every route takes the
//! token as `Authorization: Bearer`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use http::header::{AUTHORIZATION, HeaderValue};
use rig_core::DynModel;
use rig_core::catalog::{ModelFacts, ModelSpec, ReasoningSupport};
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::anthropic::wire::Auth;
use rig_core::providers::anthropic::{
    AnthropicConfig, Dialect as MessagesDialect, ToolInputStreaming, compatible,
};
use rig_core::providers::gemini::GeminiConfig;
use rig_core::providers::openai::wire::Dialect;
use rig_core::providers::openai::{OpenAIConfig, Route};
use rig_core::providers::registry::ProviderId;
use serde_json::Value;

use super::derived_facts::{facts_as, no_facts_notice, notice};
use super::device_flow::REQUEST_TIMEOUT;
use super::multi_wire::WireClients;
use super::{
    AccountClient, BaseUrlPolicy, Billing, CacheScope, ConnectContext, CredentialPolicy,
    PromptCaching, Provider, vendor,
};
use crate::config::{Account, Wire};
use crate::transport::{Decorated, Decoration, caused_client, reqwest_client};
use fontaine_tools::error_chain::chain;

/// The catalog schema whose model services the listing reads.
const LISTING_PARENT: &str = "schemas/system.ai";
const LISTING_PAGE_SIZE: &str = "100";
const LISTING_MAX_PAGES: usize = 20;
const SERVICE_PREFIX: &str = "model-services/system.ai.";

/// The api types of `supported_api_types` that carry a chat turn.
const CHAT_API_TYPES: &[&str] = &[
    "anthropic/v1/messages",
    "openai/v1/responses",
    "codex/v1/responses",
    "gemini/v1/generateContent",
    "gemini/v1/streamGenerateContent",
    "mlflow/v1/chat/completions",
    "mlflow/v1/responses",
    "openai/v1/chat/completions",
];

/// The output cap of a model family whose catalog facts we lack, by id prefix, where the
/// gateway refuses more than the generic fallback. More families are added as they are found.
const OUTPUT_CAPS: &[(&str, u32)] = &[("glm-", 25_000)];

/// Never priced: the workspace bills in DBUs. The empty key and URL are placeholders.
const DBX_DIALECT: Dialect = Dialect::gateway("databricks", "", "");

/// The Messages models behind the gateway are not all Anthropic's, so the gateway quirks
/// apply: a default `max_tokens` where Anthropic's own dialect would refuse the request, and no
/// strict tool schemas.
const DBX_MESSAGES: MessagesDialect = compatible("databricks", "", "", None);

pub struct Databricks;

impl Provider for Databricks {
    fn name(&self) -> &'static str {
        "databricks"
    }

    fn credential(&self) -> CredentialPolicy {
        CredentialPolicy::Required
    }

    fn base_url_policy(&self) -> BaseUrlPolicy {
        BaseUrlPolicy::Required
    }

    fn billing(&self) -> Billing {
        Billing::Unpriced
    }

    fn accepts_wires(&self) -> bool {
        true
    }

    fn accepts_model_facts(&self) -> bool {
        true
    }

    fn accepts_prompt_caching(&self) -> bool {
        true
    }

    fn default_prompt_caching(&self) -> bool {
        true
    }

    fn validate(&self, account: &Account) -> Result<(), (&'static str, String)> {
        let Some(url) = account.base_url.as_deref().filter(|u| !u.is_empty()) else {
            return Err((
                "base_url",
                "required for kind = \"databricks\": the workspace origin, such as \
                 `https://<workspace>`"
                    .to_string(),
            ));
        };
        let parsed = reqwest::Url::parse(url).map_err(|_| {
            (
                "base_url",
                "not a URL; expected the workspace origin, such as `https://<workspace>`"
                    .to_string(),
            )
        })?;
        if !parsed.path().trim_matches('/').is_empty()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err((
                "base_url",
                "must be the workspace origin without a path, such as `https://<workspace>`; \
                 Fontaine adds `/ai-gateway/...` itself"
                    .to_string(),
            ));
        }
        check_model_facts(account)
    }

    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String> {
        let origin = ctx
            .base_url()
            .ok_or_else(|| "a `databricks` account needs a `base_url`".to_string())?
            .trim_end_matches('/')
            .to_string();
        let key = ctx.api_key().unwrap_or_default();
        let http = caused_client(ctx.network).map_err(|e| chain(&e))?;

        let openai = |route: Route, root: &str| {
            OpenAIConfig::with_key(&DBX_DIALECT, key.as_str())
                .with_base_url(root)
                .with_route(route)
                .connect(http.clone())
        };
        let mut messages_config = AnthropicConfig::with_key(&DBX_MESSAGES, key.as_str())
            .with_base_url(format!("{origin}/ai-gateway/anthropic"));
        messages_config.dialect.quirks.auth = Auth::OptionalBearer;

        // rig's Gemini wire appends `?key=<api_key>` to every URI and has no static bearer
        // mode, so the credential rides as a fixed header and the parameter is stripped.
        let mut bearer = HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|_| "the token cannot be sent as an HTTP header".to_string())?;
        bearer.set_sensitive(true);
        let gemini_http = Decorated::new(
            http.clone(),
            Decoration {
                headers: vec![(AUTHORIZATION, bearer)],
                strip_key_param: true,
                ..Decoration::default()
            },
        );

        let wires = WireClients {
            chat: openai(Route::Chat, &format!("{origin}/ai-gateway/mlflow/v1")),
            responses: openai(Route::Responses, &format!("{origin}/ai-gateway/openai/v1")),
            messages: messages_config.connect(http.clone()),
            gemini: GeminiConfig::new("")
                .with_base_url(format!("{origin}/ai-gateway/gemini"))
                .connect(gemini_http),
            caching: PromptCaching::of(ctx.account),
        };
        Ok(Arc::new(DatabricksClient {
            wires,
            overrides: ctx.account.wires.clone().unwrap_or_default(),
            model_facts: ctx.account.model_facts.clone().unwrap_or_default(),
            account_name: ctx.account_name.to_string(),
            listing: Listing {
                client: reqwest_client(ctx.network).map_err(|e| chain(&e))?,
                url: format!("{origin}/api/2.1/unity-catalog/model-services"),
                key,
            },
        }))
    }
}

/// `model` without the gateway's catalog prefix (`system.ai.`, or `databricks-` as older
/// configs spell it).
fn base_id(model: &str) -> &str {
    model
        .strip_prefix("system.ai.")
        .or_else(|| model.strip_prefix("databricks-"))
        .unwrap_or(model)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Claude,
    Gpt,
    Gemini,
    Other,
}

fn family(base: &str) -> Family {
    if base.starts_with("claude-") {
        Family::Claude
    } else if base.starts_with("gpt-") && !base.starts_with("gpt-oss-") {
        Family::Gpt
    } else if base.starts_with("gemini-") {
        Family::Gemini
    } else {
        Family::Other
    }
}

/// The catalog vendor whose entries describe a family's models.
fn catalog_vendor_of(family: Family) -> Option<ProviderId> {
    match family {
        Family::Claude => Some(vendor("anthropic")),
        Family::Gpt => Some(vendor("openai")),
        Family::Gemini => Some(vendor("gcp.gemini")),
        Family::Other => None,
    }
}

/// The wire a model is served on: the account's override, else the model's family. Custom
/// `catalog.schema.name` services and open models take the provider-neutral Chat route.
pub fn wire(model: &str, overrides: &BTreeMap<String, Wire>) -> Wire {
    if let Some(wire) = overrides.get(model) {
        return *wire;
    }
    match family(base_id(model)) {
        Family::Claude => Wire::Messages,
        Family::Gpt => Wire::Responses,
        Family::Gemini => Wire::Gemini,
        Family::Other => Wire::Chat,
    }
}

/// What the catalog says about `model`, and the listed id derived facts came from when the
/// catalog does not list the model itself.
struct Facts {
    facts: ModelFacts,
    derived_from: Option<String>,
    /// The catalog id an account's `model_facts` mapped the model to.
    mapped_to: Option<String>,
}

fn facts_for(model: &str, mapped: &BTreeMap<String, String>) -> Option<Facts> {
    let served = ProviderId::declare("databricks").ok();
    if let Some(target) = mapped.get(model)
        && let Some(vendor) = catalog_vendor_of(family(target))
        && let Some(served) = served
        && let Some((facts, from, listed)) = facts_as(vendor, served, target, model)
    {
        return Some(Facts {
            facts,
            derived_from: (!listed).then_some(from),
            mapped_to: Some(target.clone()),
        });
    }
    let base = base_id(model);
    if let Some(vendor) = catalog_vendor_of(family(base))
        && let Some(served) = served
        && let Some((facts, from, listed)) = facts_as(vendor, served, base, model)
    {
        return Some(Facts {
            facts,
            derived_from: (!listed).then_some(from),
            mapped_to: None,
        });
    }
    let (_, cap) = OUTPUT_CAPS
        .iter()
        .find(|(prefix, _)| base.starts_with(prefix))?;
    let spec = ModelSpec::new(served?, model)
        .with_max_output_tokens(*cap)
        .with_tools(true)
        .with_reasoning(ReasoningSupport::Unknown { default: None });
    Some(Facts {
        facts: ModelFacts::new(spec),
        derived_from: None,
        mapped_to: None,
    })
}

/// The one-time notice for a model whose facts were derived from another listed model, or a
/// custom `catalog.schema.name` endpoint with no facts at all. An open model on `system.ai` has
/// none legitimately, so it is not warned about.
fn facts_notice_for(
    model: &str,
    mapped: &BTreeMap<String, String>,
    account: &str,
) -> Option<String> {
    let Some(facts) = facts_for(model, mapped) else {
        let custom = base_id(model) == model && model.contains('.');
        return custom.then(|| no_facts_notice(model, account));
    };
    let from = facts.derived_from?;
    let resolved = facts.mapped_to.as_deref().unwrap_or_else(|| base_id(model));
    Some(notice(model, resolved, &from, None))
}

/// The `model_facts` entries of `account`: a key the catalog already lists keeps its own facts,
/// and a value must resolve to a catalog model of a vendor Fontaine has facts for.
fn check_model_facts(account: &Account) -> Result<(), (&'static str, String)> {
    let served = ProviderId::declare("databricks").ok();
    for (key, target) in account.model_facts.iter().flatten() {
        let listed = catalog_vendor_of(family(base_id(key)))
            .zip(served)
            .and_then(|(vendor, served)| facts_as(vendor, served, base_id(key), key))
            .is_some_and(|(_, _, listed)| listed);
        if listed {
            return Err((
                "model_facts",
                format!(
                    "`{key}` is a model the catalog lists, which keeps its own facts; map only \
                     a name the host invented"
                ),
            ));
        }
        let resolves = catalog_vendor_of(family(target))
            .zip(served)
            .and_then(|(vendor, served)| facts_as(vendor, served, target, key))
            .is_some();
        if !resolves {
            return Err((
                "model_facts",
                format!(
                    "`{key}` maps to `{target}`, which is not a catalog Claude, GPT or Gemini model"
                ),
            ));
        }
    }
    Ok(())
}

/// How a model on the Messages wire asks for streamed tool input. Claude takes the per-tool
/// field; another provider's model forced onto Messages gets the beta flag instead.
fn tool_input_for(model: &str, wire: Wire) -> Option<ToolInputStreaming> {
    (wire == Wire::Messages && family(base_id(model)) != Family::Claude)
        .then_some(ToolInputStreaming::BetaHeader)
}

struct Listing {
    client: reqwest::Client,
    url: String,
    key: String,
}

impl Listing {
    async fn page(
        &self,
        token: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), Box<ProviderError>> {
        let mut url = reqwest::Url::parse(&self.url).map_err(|_| {
            Box::new(ProviderError::Provider(
                "the workspace URL is not valid".to_string(),
            ))
        })?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("parent", LISTING_PARENT)
                .append_pair("page_size", LISTING_PAGE_SIZE);
            if let Some(token) = token {
                query.append_pair("page_token", token);
            }
        }
        let request = self
            .client
            .get(url)
            .bearer_auth(&self.key)
            .timeout(REQUEST_TIMEOUT);
        let response = request.send().await.map_err(|e| {
            Box::new(ProviderError::Provider(format!(
                "could not fetch the model services: {}",
                chain(&e.without_url())
            )))
        })?;
        let status = response.status();
        let body = response.text().await.map_err(|e| {
            Box::new(ProviderError::Provider(format!(
                "could not read the model services: {}",
                chain(&e.without_url())
            )))
        })?;
        if !status.is_success() {
            return Err(Box::new(ProviderError::Provider(format!(
                "the model services listing replied {status}"
            ))));
        }
        parse_model_services(&body).map_err(|e| Box::new(ProviderError::Response(e)))
    }

    async fn all(&self) -> Result<Vec<String>, Box<ProviderError>> {
        let mut ids = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut token: Option<String> = None;
        for _ in 0..LISTING_MAX_PAGES {
            let (page, next) = self.page(token.as_deref()).await?;
            ids.extend(page);
            match next {
                Some(next) if seen.insert(next.clone()) => token = Some(next),
                _ => break,
            }
        }
        Ok(ids.into_iter().collect())
    }
}

/// The chat-capable `system.ai.*` ids of one page of the model-services listing, and the token
/// of the next page. `supported_api_types` is advisory: a missing or empty list keeps the
/// entry, and a list naming no chat type (embeddings) drops it.
pub(crate) fn parse_model_services(body: &str) -> Result<(Vec<String>, Option<String>), String> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| format!("the model services listing is not JSON: {e}"))?;
    let entries = value
        .get("model_services")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut ids = BTreeSet::new();
    for entry in entries {
        let Some(name) = entry.get("name").and_then(Value::as_str) else {
            continue;
        };
        if !name.starts_with(SERVICE_PREFIX) {
            continue;
        }
        let types: Vec<&str> = entry
            .get("supported_api_types")
            .and_then(Value::as_array)
            .map(|types| types.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let chat = types
            .iter()
            .any(|t| CHAT_API_TYPES.contains(&t.trim_start_matches('/')));
        if !types.is_empty() && !chat {
            continue;
        }
        ids.insert(name.trim_start_matches("model-services/").to_string());
    }
    let next = value
        .get("next_page_token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    Ok((ids.into_iter().collect(), next))
}

struct DatabricksClient {
    wires: WireClients,
    overrides: BTreeMap<String, Wire>,
    model_facts: BTreeMap<String, String>,
    account_name: String,
    listing: Listing,
}

impl DatabricksClient {
    fn completion_in(&self, model: &str, scope: CacheScope) -> DynModel<Completion> {
        let wire = wire(model, &self.overrides);
        self.wires.completion(
            wire,
            model,
            facts_for(model, &self.model_facts).map(|f| f.facts),
            scope,
            tool_input_for(model, wire),
        )
    }
}

impl AccountClient for DatabricksClient {
    fn completion(&self, model: &str) -> DynModel<Completion> {
        self.completion_in(model, CacheScope::Conversation)
    }

    fn guardian_completion(&self, model: &str) -> DynModel<Completion> {
        self.completion_in(model, CacheScope::StaticPrefix)
    }

    fn facts_notice(&self, model: &str) -> Option<String> {
        facts_notice_for(model, &self.model_facts, &self.account_name)
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>> {
        async move { self.listing.all().await }.boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(model: &str) -> Wire {
        wire(model, &BTreeMap::new())
    }

    #[test]
    fn the_table_routes_each_family() {
        assert_eq!(route("system.ai.claude-sonnet-5-5"), Wire::Messages);
        assert_eq!(route("system.ai.gpt-5-6-luna"), Wire::Responses);
        assert_eq!(route("system.ai.gpt-oss-120b"), Wire::Chat);
        assert_eq!(route("system.ai.gemini-3-pro"), Wire::Gemini);
        assert_eq!(route("system.ai.glm-5-3"), Wire::Chat);
        assert_eq!(route("main.team.router"), Wire::Chat);
        assert_eq!(route("databricks-claude-haiku-5-5"), Wire::Messages);
    }

    #[test]
    fn an_override_beats_the_table() {
        let overrides = BTreeMap::from([("system.ai.glm-5-3".to_string(), Wire::Messages)]);
        assert_eq!(wire("system.ai.glm-5-3", &overrides), Wire::Messages);
        assert_eq!(wire("system.ai.glm-5-4", &overrides), Wire::Chat);
    }

    #[test]
    fn only_a_non_claude_model_on_messages_gets_the_beta_header() {
        assert_eq!(
            tool_input_for("system.ai.glm-5-3", Wire::Messages),
            Some(ToolInputStreaming::BetaHeader)
        );
        assert_eq!(
            tool_input_for("system.ai.claude-haiku-5-5", Wire::Messages),
            None
        );
        assert_eq!(tool_input_for("system.ai.glm-5-3", Wire::Chat), None);
    }

    fn account(base_url: Option<&str>) -> Account {
        let toml = match base_url {
            Some(url) => format!("kind = \"databricks\"\nbase_url = \"{url}\"\n"),
            None => "kind = \"databricks\"\n".to_string(),
        };
        toml::from_str(&toml).expect("an account")
    }

    #[test]
    fn validate_wants_the_workspace_origin() {
        let check = |url| Databricks.validate(&account(url));
        assert!(check(Some("https://example.cloud.databricks.com")).is_ok());
        assert!(check(Some("https://example.cloud.databricks.com/")).is_ok());
        for bad in [
            "https://example.cloud.databricks.com/ai-gateway",
            "https://example.cloud.databricks.com/serving-endpoints",
        ] {
            let (key, message) = check(Some(bad)).unwrap_err();
            assert_eq!(key, "base_url", "{bad}");
            assert!(message.contains("without a path"), "{message}");
        }
        let (key, message) = check(None).unwrap_err();
        assert_eq!(key, "base_url");
        assert!(message.contains("required"), "{message}");
        assert_eq!(check(Some("")).unwrap_err().0, "base_url");
        assert_eq!(check(Some("not a url")).unwrap_err().0, "base_url");
    }

    #[test]
    fn the_listing_keeps_chat_capable_system_ai_services() {
        let body = serde_json::json!({
            "model_services": [
                {"name": "model-services/system.ai.claude-haiku-5-5",
                 "supported_api_types": ["anthropic/v1/messages"]},
                {"name": "model-services/system.ai.text-embed",
                 "supported_api_types": ["openai/v1/embeddings"]},
                {"name": "model-services/system.ai.no-types"},
                {"name": "model-services/system.ai.empty-types", "supported_api_types": []},
                {"name": "model-services/main.team.router",
                 "supported_api_types": ["mlflow/v1/chat/completions"]},
                {"name": "model-services/system.ai.claude-haiku-5-5",
                 "supported_api_types": ["anthropic/v1/messages"]},
                {"name": "model-services/system.ai.glm-5-3",
                 "supported_api_types": ["openai/v1/embeddings", "mlflow/v1/chat/completions"]},
            ],
            "next_page_token": "p2",
        })
        .to_string();
        let (ids, next) = parse_model_services(&body).unwrap();
        assert_eq!(
            ids,
            [
                "system.ai.claude-haiku-5-5",
                "system.ai.empty-types",
                "system.ai.glm-5-3",
                "system.ai.no-types",
            ]
        );
        assert_eq!(next.as_deref(), Some("p2"));
    }

    #[test]
    fn a_last_page_has_no_next_token() {
        for body in [
            r#"{"model_services": []}"#,
            r#"{"model_services": [], "next_page_token": ""}"#,
            r#"{}"#,
        ] {
            assert_eq!(
                parse_model_services(body).unwrap(),
                (vec![], None),
                "{body}"
            );
        }
        assert!(parse_model_services("nope").is_err());
    }

    fn mapped() -> BTreeMap<String, String> {
        BTreeMap::from([(
            "main.team.claude".to_string(),
            "claude-sonnet-5-5".to_string(),
        )])
    }

    fn notice_of(model: &str) -> Option<String> {
        facts_notice_for(model, &mapped(), "dbx")
    }

    #[test]
    fn a_listed_model_has_no_notice_and_a_derived_one_does() {
        assert_eq!(notice_of("system.ai.claude-haiku-5-5"), None);
        let notice = notice_of("system.ai.claude-haiku-9-9").expect("derived");
        assert!(notice.contains("claude-haiku-9-9"), "{notice}");
        assert!(notice.contains("Update Fontaine"), "{notice}");
        assert_eq!(notice_of("system.ai.glm-5-3"), None);
        assert_eq!(notice_of("system.ai.kimi-k3"), None);
        assert_eq!(notice_of("kimi-k3"), None);
    }

    #[test]
    fn a_custom_endpoint_is_warned_about_unless_mapped() {
        let notice = notice_of("main.team.router").expect("no facts");
        assert!(notice.contains("[accounts.dbx.model_facts]"), "{notice}");
        assert_eq!(notice_of("main.team.claude"), None);
    }

    #[test]
    fn a_mapped_endpoint_takes_the_target_facts_under_databricks() {
        let facts = facts_for("main.team.claude", &mapped()).expect("mapped");
        let spec = facts.facts.spec().expect("bound").clone();
        let sonnet = facts_for("system.ai.claude-sonnet-5-5", &BTreeMap::new())
            .expect("listed")
            .facts;
        let sonnet = sonnet.spec().expect("bound");
        assert_eq!(spec.id, "main.team.claude");
        assert_eq!(spec.max_output_tokens, sonnet.max_output_tokens);
        assert_eq!(spec.context_window, sonnet.context_window);
        assert!(spec.max_output_tokens.is_some());
        assert_eq!(spec.provider, sonnet.provider);
        assert!(facts_for("main.team.claude", &BTreeMap::new()).is_none());
    }

    #[test]
    fn a_family_with_a_cap_and_no_facts_gets_a_minimal_spec() {
        let facts = facts_for("system.ai.glm-5-3", &BTreeMap::new())
            .expect("capped")
            .facts;
        let spec = facts.spec().expect("bound");
        assert_eq!(spec.max_output_tokens, Some(25_000));
        assert!(spec.tools);
        assert!(spec.provider.is_declared());
        assert!(facts_for("system.ai.kimi-k3", &BTreeMap::new()).is_none());
    }
}
