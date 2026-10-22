//! Config accounts to rig provider clients, and `<account>/<model>` references to model
//! handles. One rig client per account, so two accounts of one kind keep separate
//! credentials and endpoints.

use std::fmt;
use std::path::Path;

use rig_core::DynModel;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::anthropic::{Anthropic, AnthropicConfig};
use rig_core::providers::gemini::{Gemini, GeminiConfig};
use rig_core::providers::ollama::{Ollama, OllamaConfig};
use rig_core::providers::openai::wire::{Auth, Dialect};
use rig_core::providers::openai::{OpenAI, OpenAIConfig};

use crate::config::{Account, AccountKind, Config, ConfigError, EnvLookup};
use crate::credentials::Credentials;

/// The one gateway dialect every `openai-compatible` account speaks. Its name is the vendor
/// rig prices by, so it is the kind name and never an account name: such endpoints are unpriced.
const GATEWAY: Dialect = Dialect::gateway("openai-compatible", "", "");

/// Why a model could not be resolved. The exit code tells the two apart: a config problem is
/// the user's to fix, a provider failure is the run's.
#[derive(Debug)]
pub enum ModelError {
    Config(ConfigError),
    /// A listing call to the provider failed: transport error, non-2xx, 401 or 403.
    Provider(String),
}

impl ModelError {
    pub fn exit_code(&self) -> u8 {
        match self {
            ModelError::Config(e) => e.exit_code(),
            ModelError::Provider(_) => 1,
        }
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::Config(e) => e.fmt(f),
            ModelError::Provider(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ModelError {}

impl From<ConfigError> for ModelError {
    fn from(e: ConfigError) -> Self {
        ModelError::Config(e)
    }
}

fn config_error(message: impl Into<String>) -> ModelError {
    ModelError::Config(ConfigError::model(message))
}

/// `<account>/<model>`, split at the first `/` so a model id may contain its own (`openrouter`
/// style ids) and `:`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRef {
    pub account: String,
    pub model: String,
}

impl ModelRef {
    pub fn parse(text: &str) -> Option<ModelRef> {
        let (account, model) = text.split_once('/')?;
        if account.is_empty() || model.is_empty() {
            return None;
        }
        Some(ModelRef {
            account: account.to_string(),
            model: model.to_string(),
        })
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.account, self.model)
    }
}

/// A model ready to complete against, with where it came from.
#[derive(Clone, Debug)]
pub struct ModelHandle {
    pub reference: ModelRef,
    pub kind: AccountKind,
    pub completion: DynModel<Completion>,
}

impl ModelHandle {
    pub fn new(reference: ModelRef, kind: AccountKind, completion: DynModel<Completion>) -> Self {
        ModelHandle {
            reference,
            kind,
            completion,
        }
    }

    /// Whether rig's catalog knows a price for this model; a spend cap needs one.
    pub fn priced(&self) -> bool {
        self.completion
            .spec()
            .is_some_and(|spec| spec.pricing.is_some())
    }
}

/// The guardian's model, when `[guardian] model` is set. A missing or invalid credential on its
/// account never stops startup: the guardian is then unavailable and every adjudication that
/// needs it fails closed.
#[derive(Clone, Debug)]
pub enum GuardianModel {
    Ready(ModelHandle),
    Unavailable { reference: ModelRef, reason: String },
}

impl GuardianModel {
    pub fn reference(&self) -> &ModelRef {
        match self {
            GuardianModel::Ready(handle) => &handle.reference,
            GuardianModel::Unavailable { reference, .. } => reference,
        }
    }
}

#[derive(Debug)]
pub struct Models {
    pub main: ModelHandle,
    pub guardian: Option<GuardianModel>,
}

pub struct ResolveParams<'a> {
    pub config: &'a Config,
    pub data_dir: &'a Path,
    pub env: EnvLookup<'a>,
    /// `--model`, which wins over `[model] model`.
    pub model_override: Option<&'a str>,
}

#[derive(Clone)]
enum Client {
    Anthropic(Anthropic),
    OpenAi(OpenAI),
    Gemini(Gemini),
    Ollama(Ollama),
    OpenAiCompatible(OpenAI),
}

impl Client {
    fn build(kind: AccountKind, account: &Account, credential: Option<String>) -> Client {
        let base_url = account.base_url.as_deref().filter(|u| !u.is_empty());
        match kind {
            AccountKind::Anthropic => {
                let mut config = AnthropicConfig::new(credential.unwrap_or_default());
                if let Some(url) = base_url {
                    config = config.with_base_url(url);
                }
                Client::Anthropic(config.client())
            }
            AccountKind::OpenAi => {
                let mut config = OpenAIConfig::new(credential.unwrap_or_default());
                if let Some(url) = base_url {
                    config = config.with_base_url(url);
                }
                Client::OpenAi(config.client())
            }
            AccountKind::Gemini => {
                let mut config = GeminiConfig::new(credential.unwrap_or_default());
                if let Some(url) = base_url {
                    config = config.with_base_url(url);
                }
                Client::Gemini(config.client())
            }
            AccountKind::Ollama => {
                let mut config = OllamaConfig::new();
                if let Some(url) = base_url {
                    config = config.with_base_url(url);
                }
                if let Some(key) = credential {
                    config = config.with_api_key(key);
                }
                Client::Ollama(config.client())
            }
            AccountKind::OpenAiCompatible => {
                let mut config = OpenAIConfig::with_key(&GATEWAY, credential.unwrap_or_default())
                    .with_auth(Auth::OptionalBearer);
                if let Some(url) = base_url {
                    config = config.with_base_url(url);
                }
                Client::OpenAiCompatible(config.client())
            }
        }
    }

    fn completion(&self, model: &str) -> DynModel<Completion> {
        match self {
            Client::Anthropic(c) => c.completion(model).erase(),
            Client::OpenAi(c) | Client::OpenAiCompatible(c) => c.completion(model).erase(),
            Client::Gemini(c) => c.completion(model).erase(),
            // The native route: `completion()` would target the daemon's `/v1` API.
            Client::Ollama(c) => c.native_completion(model).erase(),
        }
    }

    async fn list(&self) -> Result<Vec<String>, ProviderError> {
        let list = match self {
            Client::Anthropic(c) => c.list_models().await?,
            Client::OpenAi(c) | Client::OpenAiCompatible(c) => c.list_models().await?,
            Client::Gemini(c) => c.list_models().await?,
            Client::Ollama(c) => c.list_models().await?,
        };
        Ok(list.data.into_iter().map(|m| m.id).collect())
    }
}

/// Where an account's credential comes from, and what it resolved to.
enum Credential {
    Found(String),
    /// Nothing is configured to supply one.
    NoSource,
    /// A source is configured but yields nothing.
    Unresolved(String),
}

fn credential(
    name: &str,
    account: &Account,
    env: EnvLookup<'_>,
    stored: &Credentials,
) -> Credential {
    match &account.api_key_env {
        Some(var) => match env(var).and_then(|v| v.into_string().ok()) {
            Some(value) if !value.is_empty() => Credential::Found(value),
            _ => Credential::Unresolved(format!(
                "environment variable {var}, named by `api_key_env`, is unset or empty"
            )),
        },
        None => match stored.account(name) {
            Some(key) => Credential::Found(key.to_string()),
            None => Credential::NoSource,
        },
    }
}

/// Whether the kind needs a credential to be usable at all.
fn needs_credential(kind: AccountKind) -> bool {
    matches!(
        kind,
        AccountKind::Anthropic | AccountKind::OpenAi | AccountKind::Gemini
    )
}

fn missing_credential_hint(name: &str) -> String {
    format!("set `api_key_env` in [accounts.{name}] or run `fontaine auth login {name}`")
}

/// A client for `name`, or the reason none can be built: the credential is missing or a
/// configured source yields nothing.
fn client_for(
    name: &str,
    account: &Account,
    env: EnvLookup<'_>,
    stored: &Credentials,
) -> Result<Client, String> {
    let key = match credential(name, account, env, stored) {
        Credential::Found(key) => Some(key),
        Credential::NoSource if !needs_credential(account.kind) => None,
        Credential::NoSource => {
            return Err(format!(
                "account `{name}` has no credential; {}",
                missing_credential_hint(name)
            ));
        }
        Credential::Unresolved(why) => return Err(format!("account `{name}`: {why}")),
    };
    Ok(Client::build(account.kind, account, key))
}

fn known_accounts(config: &Config) -> String {
    if config.accounts.is_empty() {
        return "no accounts are configured; add an [accounts.<name>] table".to_string();
    }
    let names: Vec<&str> = config.accounts.keys().map(String::as_str).collect();
    format!("configured accounts: {}", names.join(", "))
}

fn account_of<'a>(
    config: &'a Config,
    reference: &ModelRef,
    what: &str,
) -> Result<&'a Account, ModelError> {
    config.accounts.get(&reference.account).ok_or_else(|| {
        config_error(format!(
            "{what} `{reference}` names unknown account `{}`; {}",
            reference.account,
            known_accounts(config)
        ))
    })
}

fn parse_reference(text: &str, what: &str) -> Result<ModelRef, ModelError> {
    ModelRef::parse(text)
        .ok_or_else(|| config_error(format!("{what} `{text}`: expected `<account>/<model>`")))
}

/// Ollama lists `name:tag`; a bare name means the `latest` tag.
fn lists(kind: AccountKind, available: &[String], model: &str) -> bool {
    available.iter().any(|id| {
        id == model || (kind == AccountKind::Ollama && id.strip_suffix(":latest") == Some(model))
    })
}

fn unknown_model(reference: &ModelRef, available: &[String]) -> ModelError {
    let mut sample: Vec<&str> = available.iter().map(String::as_str).take(8).collect();
    if available.len() > sample.len() {
        sample.push("...");
    }
    config_error(format!(
        "account `{}` does not offer model `{}`; it offers: {}",
        reference.account,
        reference.model,
        if sample.is_empty() {
            "nothing".to_string()
        } else {
            sample.join(", ")
        }
    ))
}

/// Resolves the main model and, when `[guardian] model` is set, the guardian model.
///
/// An account without a `models` list is listed through the provider, lazily and only for the
/// main model's account. The guardian's model is checked against a list only when that list is
/// known without a call to the guardian's own credentials; its availability is otherwise the
/// guardian's startup concern and fails closed.
pub async fn resolve(params: &ResolveParams<'_>) -> Result<Models, ModelError> {
    let config = params.config;
    let main_text = params
        .model_override
        .or(config.model.as_deref())
        .ok_or_else(|| {
            config_error("no main model: set `[model] model` in the settings or pass `--model <account>/<model>`")
        })?;
    let main_ref = parse_reference(main_text, "model")?;
    let main_account = account_of(config, &main_ref, "model")?;
    let stored = Credentials::load(params.data_dir)?;

    let main_client =
        client_for(&main_ref.account, main_account, params.env, &stored).map_err(config_error)?;
    let main_models = match &main_account.models {
        Some(list) => list.clone(),
        None => main_client.list().await.map_err(|e| {
            ModelError::Provider(format!(
                "cannot list the models of account `{}`: {e}",
                main_ref.account
            ))
        })?,
    };
    if !lists(main_account.kind, &main_models, &main_ref.model) {
        return Err(unknown_model(&main_ref, &main_models));
    }
    let main = ModelHandle::new(
        main_ref.clone(),
        main_account.kind,
        main_client.completion(&main_ref.model),
    );

    let guardian = match config.guardian.model.as_deref() {
        None => None,
        Some(text) => Some(
            resolve_guardian(
                params,
                &stored,
                text,
                (&main_ref, &main_client, &main_models),
            )
            .await?,
        ),
    };
    Ok(Models { main, guardian })
}

async fn resolve_guardian(
    params: &ResolveParams<'_>,
    stored: &Credentials,
    text: &str,
    main: (&ModelRef, &Client, &[String]),
) -> Result<GuardianModel, ModelError> {
    let (main_ref, main_client, main_models) = main;
    let reference = parse_reference(text, "guardian model")?;
    let account = account_of(params.config, &reference, "guardian model")?;

    let known: Option<&[String]> = match &account.models {
        Some(list) => Some(list),
        None if reference.account == main_ref.account => Some(main_models),
        None => None,
    };
    if let Some(list) = known
        && !lists(account.kind, list, &reference.model)
    {
        return Err(unknown_model(&reference, list));
    }

    let client = if reference.account == main_ref.account {
        Ok(main_client.clone())
    } else {
        client_for(&reference.account, account, params.env, stored)
    };
    Ok(match client {
        Ok(client) => {
            let completion = client.completion(&reference.model);
            GuardianModel::Ready(ModelHandle::new(reference, account.kind, completion))
        }
        Err(reason) => GuardianModel::Unavailable { reference, reason },
    })
}
