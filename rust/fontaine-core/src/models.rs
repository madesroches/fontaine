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
use crate::transport::caused_client;
use fontaine_guardian::models::{ModelEntry, ModelsFile};

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

/// The guardian's model. A missing or invalid credential on its account never stops startup:
/// the guardian is then unavailable and every adjudication that needs it fails closed.
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

/// How the guardian's model was chosen. Only an explicit choice is ever pinned; the other two
/// follow the configured accounts and the main model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardianChoice {
    /// `[guardian] model` or `/guardian-model` named it. It is never substituted.
    Pinned,
    /// The highest-ranked benchmark-validated model an account offers.
    Default,
    /// Nothing validated is available, so the main model judges.
    MainFallback,
}

#[derive(Debug)]
pub struct Models {
    pub main: ModelHandle,
    /// The model ids the main model's account listed when it was resolved.
    pub main_listing: Vec<String>,
    pub guardian: Option<GuardianModel>,
    pub guardian_choice: GuardianChoice,
    /// Things the user should be told once at startup.
    pub notices: Vec<String>,
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
    fn build(
        kind: AccountKind,
        account: &Account,
        credential: Option<String>,
    ) -> Result<Client, String> {
        let http = caused_client()?;
        let base_url = account.base_url.as_deref().filter(|u| !u.is_empty());
        match kind {
            AccountKind::Anthropic => {
                let mut config = AnthropicConfig::new(credential.unwrap_or_default());
                if let Some(url) = base_url {
                    config = config.with_base_url(url);
                }
                Ok(Client::Anthropic(config.connect(http)))
            }
            AccountKind::OpenAi => {
                let mut config = OpenAIConfig::new(credential.unwrap_or_default());
                if let Some(url) = base_url {
                    config = config.with_base_url(url);
                }
                Ok(Client::OpenAi(config.connect(http)))
            }
            AccountKind::Gemini => {
                let mut config = GeminiConfig::new(credential.unwrap_or_default());
                if let Some(url) = base_url {
                    config = config.with_base_url(url);
                }
                Ok(Client::Gemini(config.connect(http)))
            }
            AccountKind::Ollama => {
                let mut config = OllamaConfig::new();
                if let Some(url) = base_url {
                    config = config.with_base_url(url);
                }
                if let Some(key) = credential {
                    config = config.with_api_key(key);
                }
                Ok(Client::Ollama(config.connect(http)))
            }
            AccountKind::OpenAiCompatible => {
                let mut config = OpenAIConfig::with_key(&GATEWAY, credential.unwrap_or_default())
                    .with_auth(Auth::OptionalBearer);
                if let Some(url) = base_url {
                    config = config.with_base_url(url);
                }
                Ok(Client::OpenAiCompatible(config.connect(http)))
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

    async fn list(&self) -> Result<Vec<String>, Box<ProviderError>> {
        let list = match self {
            Client::Anthropic(c) => c.list_models().await.map_err(Box::new)?,
            Client::OpenAi(c) | Client::OpenAiCompatible(c) => {
                c.list_models().await.map_err(Box::new)?
            }
            Client::Gemini(c) => c.list_models().await.map_err(Box::new)?,
            Client::Ollama(c) => c.list_models().await.map_err(Box::new)?,
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
    Client::build(account.kind, account, key)
}

/// A transport error's own message rarely says why the request failed; the reason (refused,
/// reset, timed out) is further down the source chain. rig's `ProviderError::source` drops the
/// transport error, so the walk starts from it directly. Causes whose text is already shown are
/// skipped, since wrappers often repeat their source's message.
fn with_causes(error: &ProviderError) -> String {
    let mut text = error.to_string();
    let mut source = match error {
        ProviderError::Http(transport) => std::error::Error::source(&**transport),
        other => std::error::Error::source(other),
    };
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
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

/// The main model's client, its model ids and its handle.
///
/// Generic over the environment lookup so a caller whose lookup is `Sync` gets a future it can
/// send to another task.
async fn resolve_main_model<E: Fn(&str) -> Option<std::ffi::OsString> + ?Sized>(
    config: &Config,
    stored: &Credentials,
    env: &E,
    text: &str,
    what: &str,
) -> Result<(ModelRef, Client, Vec<String>, ModelHandle), ModelError> {
    let main_ref = parse_reference(text, what)?;
    let main_account = account_of(config, &main_ref, what)?;
    let main_client = {
        let lookup = |name: &str| env(name);
        client_for(&main_ref.account, main_account, &lookup, stored).map_err(config_error)?
    };
    let main_models = match &main_account.models {
        Some(list) => list.clone(),
        None => main_client.list().await.map_err(|e| {
            ModelError::Provider(format!(
                "cannot list the models of account `{}`: {}",
                main_ref.account,
                with_causes(&e)
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
    Ok((main_ref, main_client, main_models, main))
}

/// Resolves the main model and the guardian model.
///
/// An account without a `models` list is listed through the provider, lazily and only for the
/// main model's account. The guardian's model is checked against a list only when that list is
/// known without a call to the guardian's own credentials; its availability is otherwise the
/// guardian's startup concern and fails closed. With no `[guardian] model`, the default is the
/// highest-ranked validated model an account offers, and the main model when none does.
pub async fn resolve(params: &ResolveParams<'_>) -> Result<Models, ModelError> {
    resolve_with(params, &validated_models()).await
}

/// [`resolve`] against a given ranked list of validated guardian models instead of the shipped
/// one.
pub async fn resolve_with(
    params: &ResolveParams<'_>,
    validated: &[ModelEntry],
) -> Result<Models, ModelError> {
    let config = params.config;
    let main_text = params
        .model_override
        .or(config.model.as_deref())
        .ok_or_else(|| {
            config_error("no main model: set `[model] model` in the settings or pass `--model <account>/<model>`")
        })?;
    let stored = Credentials::load(params.data_dir)?;
    let (main_ref, main_client, main_models, main) =
        resolve_main_model(config, &stored, params.env, main_text, "model").await?;

    let mut notices = Vec::new();
    let (guardian, guardian_choice) = match config.guardian.model.as_deref() {
        Some(text) => (
            resolve_guardian(
                config,
                &stored,
                params.env,
                text,
                (&main_ref, &main_client, &main_models),
            )?,
            GuardianChoice::Pinned,
        ),
        None => match default_guardian(
            validated,
            config,
            &stored,
            params.env,
            (&main_ref, &main_client, &main_models),
        ) {
            Some(guardian) => (guardian, GuardianChoice::Default),
            None => {
                notices.push(format!(
                    "no benchmark-validated guardian model is available on your accounts, so the guardian runs on your main model `{main_ref}`; set `[guardian] model` or use /guardian-model to choose one"
                ));
                (
                    GuardianModel::Ready(main.clone()),
                    GuardianChoice::MainFallback,
                )
            }
        },
    };
    Ok(Models {
        main,
        main_listing: main_models,
        guardian: Some(guardian),
        guardian_choice,
        notices,
    })
}

fn resolve_guardian(
    config: &Config,
    stored: &Credentials,
    env: EnvLookup<'_>,
    text: &str,
    main: (&ModelRef, &Client, &[String]),
) -> Result<GuardianModel, ModelError> {
    let (main_ref, main_client, main_models) = main;
    build_guardian(
        config,
        stored,
        env,
        text,
        Some((main_ref, main_models)),
        Some(main_client),
    )
}

/// The guardian `text` names. The main model's account listing vouches for a guardian on that
/// account, and reuses the main client rather than building another.
fn build_guardian(
    config: &Config,
    stored: &Credentials,
    env: EnvLookup<'_>,
    text: &str,
    main: Option<(&ModelRef, &[String])>,
    main_client: Option<&Client>,
) -> Result<GuardianModel, ModelError> {
    let reference = parse_reference(text, "guardian model")?;
    let account = account_of(config, &reference, "guardian model")?;
    let same_account = main.is_some_and(|(main_ref, _)| reference.account == main_ref.account);

    let known: Option<&[String]> = match (&account.models, main) {
        (Some(list), _) => Some(list),
        (None, Some((_, listing))) if same_account => Some(listing),
        _ => None,
    };
    if let Some(list) = known
        && !lists(account.kind, list, &reference.model)
    {
        return Err(unknown_model(&reference, list));
    }

    let client = match main_client {
        Some(client) if same_account => Ok(client.clone()),
        _ => client_for(&reference.account, account, env, stored),
    };
    Ok(match client {
        Ok(client) => {
            let completion = client.completion(&reference.model);
            GuardianModel::Ready(ModelHandle::new(reference, account.kind, completion))
        }
        Err(reason) => GuardianModel::Unavailable { reference, reason },
    })
}

/// The shipped list of validated guardian models, best first.
pub fn validated_models() -> Vec<ModelEntry> {
    let file: ModelsFile = toml::from_str(fontaine_guardian::models::MODELS_TOML)
        .expect("the shipped validated-models table parses");
    fontaine_guardian::models::ranked(&file.models)
}

/// Whether `account` offers `model`, judged without a call to a guardian account: by its
/// explicit `models` list, by the listing already fetched for the main account, or, for any
/// other account that lists nothing, by its kind matching and a credential being present.
fn account_has(
    name: &str,
    account: &Account,
    model: &str,
    main: (&ModelRef, &[String]),
    env: EnvLookup<'_>,
    stored: &Credentials,
) -> bool {
    if needs_credential(account.kind)
        && !matches!(credential(name, account, env, stored), Credential::Found(_))
    {
        return false;
    }
    match &account.models {
        Some(list) => lists(account.kind, list, model),
        None if name == main.0.account => lists(account.kind, main.1, model),
        None => matches!(credential(name, account, env, stored), Credential::Found(_)),
    }
}

/// The validated guardian models the configured accounts offer, best first.
fn available_validated(
    validated: &[ModelEntry],
    config: &Config,
    stored: &Credentials,
    env: EnvLookup<'_>,
    main: (&ModelRef, &[String]),
) -> Vec<ModelRef> {
    let mut out = Vec::new();
    for entry in validated {
        for (name, account) in &config.accounts {
            if account.kind.as_str() == entry.kind
                && account_has(name, account, &entry.model, main, env, stored)
            {
                out.push(ModelRef {
                    account: name.clone(),
                    model: entry.model.clone(),
                });
            }
        }
    }
    out
}

fn default_guardian(
    validated: &[ModelEntry],
    config: &Config,
    stored: &Credentials,
    env: EnvLookup<'_>,
    main: (&ModelRef, &Client, &[String]),
) -> Option<GuardianModel> {
    available_validated(validated, config, stored, env, (main.0, main.2))
        .into_iter()
        .find_map(|reference| {
            let text = reference.to_string();
            match resolve_guardian(config, stored, env, &text, main) {
                Ok(ready @ GuardianModel::Ready(_)) => Some(ready),
                _ => None,
            }
        })
}

/// Resolves models again while a session runs, for `/model` and `/guardian-model`. It owns what
/// it needs, so it can be used from an interface that holds no borrow of the startup state.
#[derive(Clone)]
pub struct Switcher {
    pub config: Config,
    pub data_dir: std::path::PathBuf,
    pub env: SharedEnv,
}

pub type SharedEnv = std::sync::Arc<dyn Fn(&str) -> Option<std::ffi::OsString> + Send + Sync>;

impl Switcher {
    pub fn new(config: Config, data_dir: std::path::PathBuf, env: SharedEnv) -> Switcher {
        Switcher {
            config,
            data_dir,
            env,
        }
    }

    /// The main model `text` names, checked against what its account offers, and that listing.
    pub async fn main(&self, text: &str) -> Result<(ModelHandle, Vec<String>), ModelError> {
        let stored = Credentials::load(&self.data_dir)?;
        let (_, _, listing, handle) =
            resolve_main_model(&self.config, &stored, &*self.env, text, "model").await?;
        Ok((handle, listing))
    }

    /// The guardian model `text` names. An account without a credential gives an unavailable
    /// guardian, not an error: it fails closed and the user is asked what to do. `main` is the
    /// main model and the listing of its account, which vouches for a guardian on that account.
    pub fn guardian(
        &self,
        text: &str,
        main: Option<(&ModelRef, &[String])>,
    ) -> Result<GuardianModel, ModelError> {
        let stored = Credentials::load(&self.data_dir)?;
        build_guardian(&self.config, &stored, &*self.env, text, main, None)
    }

    /// The model `$guardian_default` stands for: the top-ranked validated guardian model an
    /// account offers, or `None` when none is available.
    pub fn guardian_default(&self, main: &ModelRef, main_models: &[String]) -> Option<ModelHandle> {
        let stored = Credentials::load(&self.data_dir).ok()?;
        available_validated(
            &validated_models(),
            &self.config,
            &stored,
            &*self.env,
            (main, main_models),
        )
        .into_iter()
        .find_map(|reference| {
            match build_guardian(
                &self.config,
                &stored,
                &*self.env,
                &reference.to_string(),
                Some((main, main_models)),
                None,
            ) {
                Ok(GuardianModel::Ready(handle)) => Some(handle),
                _ => None,
            }
        })
    }

    /// Guardian models to offer when the pinned one is unavailable: the validated list entries
    /// the accounts offer, then every model an account lists explicitly.
    pub fn candidates(&self, main: &ModelRef, main_models: &[String]) -> Vec<ModelRef> {
        let Ok(stored) = Credentials::load(&self.data_dir) else {
            return Vec::new();
        };
        let mut out = available_validated(
            &validated_models(),
            &self.config,
            &stored,
            &*self.env,
            (main, main_models),
        );
        for (name, account) in &self.config.accounts {
            if !matches!(
                credential(name, account, &*self.env, &stored),
                Credential::Found(_)
            ) && needs_credential(account.kind)
            {
                continue;
            }
            for model in account.models.iter().flatten() {
                let reference = ModelRef {
                    account: name.clone(),
                    model: model.clone(),
                };
                if !out.contains(&reference) {
                    out.push(reference);
                }
            }
        }
        out
    }
}
