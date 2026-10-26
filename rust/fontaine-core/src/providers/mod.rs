//! The account kinds. Each kind is one module implementing [`Provider`]; resolution, login and
//! listing code only ever talk to the trait, so a new kind is a module and one registry line.

use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;
use rig_core::DynModel;
use rig_core::driver::Model;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::anthropic::Messages;
use rig_core::providers::anthropic::completion::CacheTtl as RigTtl;

use crate::config::{Account, CacheTtl};
use crate::credentials::StoredCredential;
use fontaine_tools::tls::LoadedNetwork;

pub mod anthropic;
pub mod device_flow;
pub mod gemini;
pub mod ollama;
pub mod openai;
pub mod openai_compatible;
pub mod opencode_go;
pub mod token_source;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialPolicy {
    /// The account is unusable without one.
    Required,
    /// A keyless endpoint is normal; a key is sent when present.
    Optional,
    /// The credential comes from the kind's interactive sign-in (`/login`).
    SignIn,
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

    /// Whether requests carry cache breakpoints when the account sets no `prompt_caching`.
    fn default_prompt_caching(&self) -> bool {
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
    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>>;
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

pub struct ConnectContext<'a> {
    pub account_name: &'a str,
    pub account: &'a Account,
    pub credential: Option<StoredCredential>,
    pub data_dir: &'a Path,
    pub session_id: Option<&'a str>,
    pub network: &'a LoadedNetwork,
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
    &ollama::Ollama,
    &opencode_go::OpencodeGo,
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
}
