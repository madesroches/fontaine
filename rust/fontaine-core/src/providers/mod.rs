//! The account kinds. Each kind is one module implementing [`Provider`]; resolution, login and
//! listing code only ever talk to the trait, so a new kind is a module and one registry line.

use std::path::Path;
use std::sync::Arc;

use fontaine_guardian::OutputProbe;
use futures::future::BoxFuture;
use rig_core::DynModel;
use rig_core::driver::Model;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::anthropic::Messages;
use rig_core::providers::anthropic::completion::CacheTtl as RigTtl;
use rig_core::providers::registry::ProviderId;

use crate::config::{Account, CacheTtl};
use crate::credentials::StoredCredential;
use crate::models::SharedEnv;
use crate::quota::QuotaSource;
use fontaine_tools::tls::LoadedNetwork;

pub mod anthropic;
pub mod cache_minimum;
mod cache_wire;
pub mod claude_subscription;
pub mod derived_facts;
pub mod device_flow;
pub(crate) mod gateway;
pub mod gemini;
pub mod kimi;
pub mod kimi_quota;
pub mod ollama;
pub mod openai;
pub mod openai_compatible;
pub mod opencode_go;
pub mod openrouter;
mod reasoning_parts;
pub mod token_source;
pub mod warn;

pub(crate) const WEB_SEARCH_TYPE: &str = "web_search_20250305";
pub(crate) const WEB_SEARCH_NAME: &str = "web_search";

/// The Anthropic server-side search entry, shared by every kind that sends it so the request
/// and the Claude CLI adapter's acceptance cannot drift apart.
pub(crate) fn anthropic_web_search() -> serde_json::Value {
    serde_json::json!({"type": WEB_SEARCH_TYPE, "name": WEB_SEARCH_NAME})
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialPolicy {
    /// The account is unusable without one.
    Required,
    /// A keyless endpoint is normal; a key is sent when present.
    Optional,
    /// The credential comes from the kind's interactive sign-in (`/login`).
    SignIn,
    /// An external program owns the login; Fontaine stores, reads and forwards nothing.
    ExternalCli,
}

/// Whether the provider dialog asks for a `base_url` when it adds an account of the kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseUrlPolicy {
    Required,
    Optional,
    /// The kind has one endpoint; the dialog does not ask.
    Fixed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Billing {
    /// rig's catalog may know a price for the model.
    Catalog,
    /// Usage is billed outside per-token prices: any cost a response carries is dropped.
    Unpriced,
}

pub trait Provider: Send + Sync + 'static {
    /// The `kind` value in settings.
    fn name(&self) -> &'static str;

    fn credential(&self) -> CredentialPolicy;

    fn billing(&self) -> Billing {
        Billing::Catalog
    }

    /// Account-shape checks beyond serde: a required `base_url`, keys this kind refuses. The
    /// error names the offending key under `accounts.<name>` and says what is wrong with it.
    fn validate(&self, _account: &Account) -> Result<(), (&'static str, String)> {
        Ok(())
    }

    /// Where the kind's requests go when the account sets no `base_url`; `None` when the kind
    /// has no endpoint of its own.
    fn default_endpoint(&self) -> Option<&'static str> {
        None
    }

    /// The server-side search tool this kind's API accepts, as the entry the request's `tools`
    /// array carries; `None` when the kind has none.
    fn native_web_search(&self) -> Option<serde_json::Value> {
        None
    }

    fn base_url_policy(&self) -> BaseUrlPolicy {
        BaseUrlPolicy::Fixed
    }

    /// Whether the account may set `wires`.
    fn accepts_wires(&self) -> bool {
        false
    }

    /// Whether the account may set `context_tokens` and `think`.
    fn accepts_model_options(&self) -> bool {
        false
    }

    /// Whether the account may set `prompt_caching` and `cache_ttl`.
    fn accepts_prompt_caching(&self) -> bool {
        false
    }

    /// Whether the account may set `quota`.
    fn accepts_quota(&self) -> bool {
        false
    }

    /// Whether requests carry cache breakpoints when the account sets no `prompt_caching`.
    fn default_prompt_caching(&self) -> bool {
        false
    }

    /// Whether a listing is needed before any model of the kind can be built, because the model
    /// facts come from the listing. A configured `models` list then only filters it.
    fn requires_discovery(&self) -> bool {
        false
    }

    /// Whether a listed id satisfies a requested model.
    fn lists(&self, available: &[String], model: &str) -> bool {
        available.iter().any(|id| id == model)
    }

    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String>;

    /// The sign-in `/login` runs instead of prompting for a key.
    fn sign_in(&self) -> Option<&dyn SignIn> {
        None
    }

    /// The program that signs an external-login account in, for a host that can hand it the
    /// terminal; `None` when the kind has no such program.
    fn external_login(&self, _account: &Account, _env: &SharedEnv) -> Option<ExternalLogin> {
        None
    }
}

/// An external program's own sign-in, to run with the terminal's stdin and stdout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalLogin {
    pub program: std::path::PathBuf,
    pub args: Vec<std::ffi::OsString>,
    /// Variables set on top of the inherited environment.
    pub env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    /// The command line as a person would type it.
    pub display: String,
}

impl ExternalLogin {
    /// The command with inherited environment and stdio: the program talks to the terminal
    /// itself, so it gets no process group of its own and no piped streams.
    pub fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(&self.program);
        command.args(&self.args).envs(self.env.iter().cloned());
        command
    }
}

/// What a sign-in tells its host while it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignInStep {
    /// The user has to open `verification_url` and confirm `user_code`.
    Code {
        verification_url: String,
        user_code: String,
    },
    /// The code is shown; the sign-in is polling for the approval.
    Waiting,
    /// Something went wrong that did not stop the sign-in.
    Warning(String),
}

/// Where a sign-in reports its steps.
pub type SignInProgress<'a> = &'a (dyn Fn(SignInStep) + Send + Sync);

pub trait SignIn: Send + Sync {
    /// Runs the interactive sign-in, reporting through `progress` and never printing; returns
    /// the summary line.
    fn sign_in<'a>(
        &'a self,
        account_name: &'a str,
        account: &'a Account,
        data_dir: &'a Path,
        network: &'a LoadedNetwork,
        progress: SignInProgress<'a>,
    ) -> BoxFuture<'a, Result<String, SignInError>>;
}

/// Why a sign-in did not finish. Never carries a token.
#[derive(Debug)]
pub struct SignInError(pub Box<dyn std::error::Error + Send + Sync>);

impl SignInError {
    /// The credentials-file error behind this failure, so a caller can treat a broken file as a
    /// configuration problem.
    pub fn into_config(self) -> Result<crate::config::ConfigError, SignInError> {
        self.0
            .downcast::<crate::config::ConfigError>()
            .map(|e| *e)
            .map_err(SignInError)
    }
}

impl std::fmt::Display for SignInError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for SignInError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

pub trait AccountClient: Send + Sync {
    fn completion(&self, model: &str) -> DynModel<Completion>;
    /// [`AccountClient::completion`] for the agent `namespace`, for kinds that derive per-caller
    /// state from it. The default ignores it.
    fn completion_for_agent(&self, model: &str, _namespace: &str) -> DynModel<Completion> {
        self.completion(model)
    }
    /// [`AccountClient::guardian_completion`] for the agent `namespace`.
    fn guardian_completion_for_agent(&self, model: &str, _namespace: &str) -> DynModel<Completion> {
        self.guardian_completion(model)
    }
    /// Why `model` is not usable although a listing named it or would have, when the kind knows.
    fn unavailable_reason(&self, _model: &str) -> Option<String> {
        None
    }
    /// The models the last [`AccountClient::list`] saw and refused, with why, in the order the
    /// endpoint listed them.
    fn refused(&self) -> Vec<RefusedModel> {
        Vec::new()
    }
    /// The catalog vendor whose listed models this account's ids are looked up under, and whose
    /// models an unlisted id takes derived facts from; `None` when the kind's ids are not a
    /// catalog vendor's.
    fn catalog_vendor(&self) -> Option<ProviderId> {
        None
    }
    /// A warning to show once when `model` resolves, for a model whose facts are not the
    /// catalog's own.
    fn facts_notice(&self, model: &str) -> Option<String> {
        let derived = derived_facts::unlisted(self.catalog_vendor()?, model)?;
        Some(derived_facts::notice(model, model, &derived.from, None))
    }
    /// What the request body must carry to turn `model`'s thinking off, where its wire needs it:
    /// the guardian and the scanner force a tool call, which a thinking model refuses.
    fn thinking_off(&self, _model: &str) -> Option<serde_json::Value> {
        None
    }
    /// The guardian's completion model. Its final message is per-call and never reused, so only
    /// the static prefix may carry a cache breakpoint.
    fn guardian_completion(&self, model: &str) -> DynModel<Completion> {
        self.completion(model)
    }
    /// How to learn which transport the guardian and scanner can use to get a typed answer
    /// from `model`. `None` when the kind's models all take a forced tool call.
    fn output_probe(&self, _model: &str) -> Option<Arc<dyn OutputProbe>> {
        None
    }
    /// Where the provider's own subscription quota can be read; `None` when the account did not
    /// opt in or its kind has none.
    fn quota_source(&self) -> Option<Arc<dyn QuotaSource>> {
        None
    }
    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>>;
    /// What the endpoint says about `model` beyond its id, when it offers it. `Err` carries why
    /// the lookup failed: this is provenance for a record and never gates a run.
    fn served_model(&self, _model: &str) -> BoxFuture<'_, Result<Option<ServedModel>, String>> {
        Box::pin(std::future::ready(Ok(None)))
    }
}

/// A model an endpoint lists that cannot be used, with the reason a selection is refused for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefusedModel {
    pub id: String,
    pub reason: String,
}

/// The provider's own `/models` entry for a model: an alias can move, so a record that names
/// only the alias cannot be tied to what answered.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServedModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
}

/// The cache settings of one account, resolved against its kind's default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptCaching {
    pub enabled: bool,
    pub ttl: CacheTtl,
}

impl PromptCaching {
    pub(crate) fn of(account: &Account) -> Self {
        PromptCaching {
            enabled: account
                .prompt_caching
                .unwrap_or_else(|| account.kind.provider().default_prompt_caching()),
            ttl: account.cache_ttl.unwrap_or_default(),
        }
    }

    /// The Messages wire with this account's breakpoints: the whole request for the main agent,
    /// only the tools and system prompt for the guardian.
    pub(crate) fn apply(self, mut model: Model<Messages>, scope: CacheScope) -> Model<Messages> {
        if !self.enabled {
            return model;
        }
        let wire = model.wire;
        let wire = match scope {
            CacheScope::Conversation => wire.with_prompt_caching(),
            CacheScope::StaticPrefix => wire,
        };
        // Always explicit for the guardian, where the prefix marker is the only one.
        let wire = match (scope, self.ttl) {
            (_, CacheTtl::OneHour) => wire.with_static_prefix_cache_ttl(RigTtl::OneHour),
            (CacheScope::StaticPrefix, CacheTtl::FiveMinutes) => {
                wire.with_static_prefix_cache_ttl(RigTtl::FiveMinutes)
            }
            (CacheScope::Conversation, CacheTtl::FiveMinutes) => wire,
        };
        model.wire = wire;
        model
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CacheScope {
    Conversation,
    StaticPrefix,
}

/// The id of a catalog vendor rig lists, by its catalog name.
pub(crate) fn vendor(name: &str) -> ProviderId {
    ProviderId::catalog(name).expect("rig's catalog lists every vendor a kind names")
}

pub struct ConnectContext<'a> {
    pub account_name: &'a str,
    pub account: &'a Account,
    pub credential: Option<StoredCredential>,
    pub data_dir: &'a Path,
    pub session_id: Option<&'a str>,
    pub network: &'a LoadedNetwork,
    /// The process environment, looked up live on every call. Owned so a client can keep it for
    /// the requests it makes later.
    pub env: SharedEnv,
    pub warn: warn::Warn,
}

impl ConnectContext<'_> {
    /// The API key, when the credential is one.
    pub(crate) fn api_key(&self) -> Option<String> {
        match &self.credential {
            Some(StoredCredential::ApiKey(key)) => Some(key.clone()),
            _ => None,
        }
    }

    pub(crate) fn base_url(&self) -> Option<&str> {
        self.account.base_url.as_deref().filter(|u| !u.is_empty())
    }
}

const PROVIDERS: &[&dyn Provider] = &[
    &anthropic::Anthropic,
    &openai::OpenAi,
    &gemini::Gemini,
    &openai_compatible::OpenAiCompatible,
    &kimi::Kimi,
    &ollama::Ollama,
    &opencode_go::OpencodeGo,
    &openrouter::OpenRouter,
    &claude_subscription::ClaudeSubscription,
];

pub fn find(name: &str) -> Option<&'static dyn Provider> {
    PROVIDERS.iter().copied().find(|p| p.name() == name)
}

pub fn names() -> impl Iterator<Item = &'static str> {
    PROVIDERS.iter().map(|p| p.name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_found() {
        let all: Vec<&str> = names().collect();
        let mut sorted = all.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len());
        for name in all {
            assert_eq!(find(name).map(|p| p.name()), Some(name));
        }
        assert!(find("nope").is_none());
    }

    struct NoOverride;

    impl AccountClient for NoOverride {
        fn completion(&self, _model: &str) -> DynModel<Completion> {
            unreachable!("the test never completes")
        }

        fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>> {
            Box::pin(std::future::ready(Ok(Vec::new())))
        }
    }

    #[tokio::test]
    async fn a_kind_without_an_override_serves_no_entry() {
        assert_eq!(NoOverride.served_model("m").await, Ok(None));
    }
}
