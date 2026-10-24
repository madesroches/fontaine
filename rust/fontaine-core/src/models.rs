//! Config accounts to rig provider clients, and `<account>/<model>` references to model
//! handles. One rig client per account, so two accounts of one kind keep separate
//! credentials and endpoints.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use rig_core::DynModel;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;

use crate::config::{Account, AccountKind, Config, ConfigError, EnvLookup};
use crate::credentials::{Credentials, StoredCredential};
use crate::providers::{AccountClient, Billing, ConnectContext, CredentialPolicy};
use fontaine_guardian::WireIds;
use fontaine_guardian::models::{ModelEntry, ModelsFile};

type Client = Arc<dyn AccountClient>;

/// What a client is built from besides the account: the stored credentials, where the data
/// directory is and the session the requests belong to.
struct Sources<'a> {
    stored: Credentials,
    data_dir: &'a Path,
    session_id: Option<&'a str>,
}

impl<'a> Sources<'a> {
    fn load(data_dir: &'a Path, session_id: Option<&'a str>) -> Result<Self, ConfigError> {
        Ok(Sources {
            stored: Credentials::load(data_dir)?,
            data_dir,
            session_id,
        })
    }
}

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
    /// Request parameters that turn the model's thinking off, for the calls that force a tool
    /// call (the guardian and the scanner).
    pub thinking_off: Option<serde_json::Value>,
}

impl ModelHandle {
    pub fn new(reference: ModelRef, kind: AccountKind, completion: DynModel<Completion>) -> Self {
        ModelHandle {
            reference,
            kind,
            completion,
            thinking_off: None,
        }
    }

    /// A handle on `reference`, completing through `client`.
    pub fn from_client(reference: ModelRef, kind: AccountKind, client: &dyn AccountClient) -> Self {
        let completion = client.completion(&reference.model);
        let thinking_off = client.thinking_off(&reference.model);
        ModelHandle {
            reference,
            kind,
            completion,
            thinking_off,
        }
    }

    /// The wire's model id and provider name, as `api_request` records them.
    pub fn wire_ids(&self) -> WireIds {
        WireIds {
            provider: self.completion.name().to_string(),
            model: self.completion.id().unwrap_or("unknown").to_string(),
        }
    }

    /// Whether the cost a call reports is counted. An unpriced kind is billed outside per-token
    /// prices, so whatever cost reaches us for it is dropped.
    pub fn catalog_billed(&self) -> bool {
        self.kind.provider().billing() == Billing::Catalog
    }

    /// Whether rig's catalog knows a price for this model; a spend cap needs one.
    pub fn priced(&self) -> bool {
        self.catalog_billed()
            && self
                .completion
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
    /// Sent to gateways that ask which session a request belongs to.
    pub session_id: Option<String>,
}

/// Where an account's credential comes from, and what it resolved to.
pub(crate) enum Credential {
    Found(StoredCredential),
    /// Nothing is configured to supply one.
    NoSource,
    /// A source is configured but yields nothing.
    Unresolved(String),
}

pub(crate) fn credential(
    name: &str,
    account: &Account,
    env: EnvLookup<'_>,
    stored: &Credentials,
) -> Credential {
    match &account.api_key_env {
        Some(var) => match env(var).and_then(|v| v.into_string().ok()) {
            Some(value) if !value.is_empty() => Credential::Found(StoredCredential::ApiKey(value)),
            _ => Credential::Unresolved(format!(
                "environment variable {var}, named by `api_key_env`, is unset or empty"
            )),
        },
        None => match stored.account(name) {
            // A sign-in left by a changed `kind` is no key for a provider that takes keys.
            Some(StoredCredential::OAuth(_))
                if account.kind.provider().credential() != CredentialPolicy::SignIn =>
            {
                Credential::NoSource
            }
            // A key left by a changed `kind` is no sign-in for a provider that needs one.
            Some(StoredCredential::ApiKey(_))
                if account.kind.provider().credential() == CredentialPolicy::SignIn =>
            {
                Credential::NoSource
            }
            Some(found) => Credential::Found(found),
            None => Credential::NoSource,
        },
    }
}

/// Whether the kind cannot be used without a credential.
fn needs_credential(kind: AccountKind) -> bool {
    kind.provider().credential() != CredentialPolicy::Optional
}

fn missing_credential_hint(name: &str) -> String {
    format!("set `api_key_env` in [accounts.{name}] or run `/login {name}` in `fontaine`")
}

/// A client for `name`, or the reason none can be built: the credential is missing or a
/// configured source yields nothing.
fn client_for(
    name: &str,
    account: &Account,
    env: EnvLookup<'_>,
    src: &Sources<'_>,
) -> Result<Client, String> {
    let provider = account.kind.provider();
    let credential = match credential(name, account, env, &src.stored) {
        Credential::Found(found) => Some(found),
        Credential::NoSource => match provider.credential() {
            CredentialPolicy::Optional => None,
            CredentialPolicy::Required => {
                return Err(format!(
                    "account `{name}` has no credential; {}",
                    missing_credential_hint(name)
                ));
            }
            CredentialPolicy::SignIn => {
                return Err(format!(
                    "account `{name}` is not signed in; run `/login {name}` in `fontaine`"
                ));
            }
        },
        Credential::Unresolved(why) => return Err(format!("account `{name}`: {why}")),
    };
    provider.connect(ConnectContext {
        account_name: name,
        account,
        credential,
        data_dir: src.data_dir,
        session_id: src.session_id,
    })
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

fn lists(kind: AccountKind, available: &[String], model: &str) -> bool {
    kind.provider().lists(available, model)
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
    src: &Sources<'_>,
    env: &E,
    text: &str,
    what: &str,
) -> Result<(ModelRef, Client, Vec<String>, ModelHandle), ModelError> {
    let main_ref = parse_reference(text, what)?;
    let main_account = account_of(config, &main_ref, what)?;
    let main_client = {
        let lookup = |name: &str| env(name);
        client_for(&main_ref.account, main_account, &lookup, src).map_err(config_error)?
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
    let main = ModelHandle::from_client(main_ref.clone(), main_account.kind, main_client.as_ref());
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
    let stored = Sources::load(params.data_dir, params.session_id.as_deref())?;
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
    stored: &Sources<'_>,
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
    stored: &Sources<'_>,
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
        Ok(client) => GuardianModel::Ready(ModelHandle::from_client(
            reference,
            account.kind,
            client.as_ref(),
        )),
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
    stored: &Sources<'_>,
) -> bool {
    let stored = &stored.stored;
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
    stored: &Sources<'_>,
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
    stored: &Sources<'_>,
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

/// How long one account may take to list its models before the picker shows it as failed.
pub const LISTING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// One configured account's models, or why they could not be listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountListing {
    pub account: String,
    pub kind: AccountKind,
    pub models: Result<Vec<String>, String>,
}

/// Resolves models again while a session runs, for `/model` and `/guardian-model`. It owns what
/// it needs, so it can be used from an interface that holds no borrow of the startup state.
#[derive(Clone)]
pub struct Switcher {
    pub config: Config,
    pub data_dir: std::path::PathBuf,
    pub env: SharedEnv,
    pub session_id: Option<String>,
}

/// The switcher a session and its subagents share, so an account added or removed mid-session
/// reaches both.
pub type SharedSwitcher = Arc<std::sync::RwLock<Switcher>>;

pub type SharedEnv = std::sync::Arc<dyn Fn(&str) -> Option<std::ffi::OsString> + Send + Sync>;

impl Switcher {
    pub fn new(
        config: Config,
        data_dir: std::path::PathBuf,
        env: SharedEnv,
        session_id: Option<String>,
    ) -> Switcher {
        Switcher {
            config,
            data_dir,
            env,
            session_id,
        }
    }

    fn sources(&self) -> Result<Sources<'_>, ConfigError> {
        Sources::load(&self.data_dir, self.session_id.as_deref())
    }

    /// The main model `text` names, checked against what its account offers, and that listing.
    pub async fn main(&self, text: &str) -> Result<(ModelHandle, Vec<String>), ModelError> {
        let stored = self.sources()?;
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
        let stored = self.sources()?;
        build_guardian(&self.config, &stored, &*self.env, text, main, None)
    }

    /// The model `$guardian_default` stands for: the top-ranked validated guardian model an
    /// account offers, or `None` when none is available.
    pub fn guardian_default(&self, main: &ModelRef, main_models: &[String]) -> Option<ModelHandle> {
        let stored = self.sources().ok()?;
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

    /// The models of every account, or only of those named in `only`, sorted by account name.
    /// An explicit `models` list and the main account's already-fetched `main` listing (none
    /// before a session has a main model) cost no call; the others are listed concurrently. A
    /// failure keeps the account's entry.
    pub async fn listings(
        &self,
        main: Option<(&ModelRef, &[String])>,
        only: Option<&[String]>,
    ) -> Vec<AccountListing> {
        self.listings_within(main, only, LISTING_TIMEOUT).await
    }

    /// Tests only: [`Switcher::listings`] with its own per-account timeout.
    pub async fn listings_within(
        &self,
        main: Option<(&ModelRef, &[String])>,
        only: Option<&[String]>,
        timeout: std::time::Duration,
    ) -> Vec<AccountListing> {
        let wanted = self
            .config
            .accounts
            .iter()
            .filter(|(name, _)| only.is_none_or(|only| only.contains(name)));
        let stored = match self.sources() {
            Ok(stored) => stored,
            Err(e) => {
                let reason = e.to_string();
                return wanted
                    .map(|(name, account)| AccountListing {
                        account: name.clone(),
                        kind: account.kind,
                        models: Err(reason.clone()),
                    })
                    .collect();
            }
        };
        let stored = &stored;
        futures::future::join_all(wanted.map(|(name, account)| async move {
            let models = if let Some(list) = &account.models {
                Ok(list.clone())
            } else if let Some(main) = main.filter(|main| *name == main.0.account) {
                Ok(main.1.to_vec())
            } else {
                match client_for(name, account, &*self.env, stored) {
                    Err(reason) => Err(reason),
                    Ok(client) => match tokio::time::timeout(timeout, client.list()).await {
                        Ok(Ok(models)) => Ok(models),
                        Ok(Err(e)) => Err(with_causes(&e)),
                        Err(_) => Err(format!("listing timed out after {timeout:?}")),
                    },
                }
            };
            AccountListing {
                account: name.clone(),
                kind: account.kind,
                models,
            }
        }))
        .await
    }

    /// Guardian models to offer when the pinned one is unavailable: the validated list entries
    /// the accounts offer, then every model an account lists explicitly.
    pub fn candidates(&self, main: &ModelRef, main_models: &[String]) -> Vec<ModelRef> {
        let Ok(stored) = self.sources() else {
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
                credential(name, account, &*self.env, &stored.stored),
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
