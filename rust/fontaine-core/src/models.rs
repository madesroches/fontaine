//! Config accounts to rig provider clients, and `<account>/<model>` references to model
//! handles. One rig client per account, so two accounts of one kind keep separate
//! credentials and endpoints.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use rig_core::DynModel;
use rig_core::completion::{CheckError, CompletionRequest, GenerationOptions, UnsupportedOption};
use rig_core::error::ProviderError;
use rig_core::operation::Completion;

use crate::config::{Account, AccountKind, Config, ConfigError, EnvLookup};
use crate::credentials::{Credentials, StoredCredential};
use crate::effort::Effort;
use crate::gate::SessionState;
use crate::providers::{
    AccountClient, Billing, ConnectContext, CredentialPolicy, RefusedModel, ServedModel,
};
use crate::quota::QuotaSource;
use fontaine_guardian::models::{ModelEntry, ModelsFile};
use fontaine_guardian::{OutputProbe, WireIds};
use fontaine_tools::tls::LoadedNetwork;

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
    /// How to learn whether the model takes a forced tool call for the guardian's typed answer;
    /// `None` when it always does.
    pub output_probe: Option<Arc<dyn OutputProbe>>,
    /// Where the account's own subscription quota is read, when it opted in.
    pub quota: Option<Arc<dyn QuotaSource>>,
    /// The same model's completions with conversation-wide and static-prefix-only caching, when
    /// it was built from a client.
    scopes: Option<Scopes>,
    /// The client the handle was built from, so a completion can be rebuilt for another caller.
    client: Option<ClientRef>,
    /// A warning to show once when the handle's model is resolved: its facts are not the
    /// catalog's own.
    pub facts_notice: Option<String>,
}

/// The client a handle was built from; only its presence shows in debug output.
#[derive(Clone)]
struct ClientRef(Client);

impl fmt::Debug for ClientRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Client")
    }
}

#[derive(Clone, Debug)]
struct Scopes {
    conversation: DynModel<Completion>,
    guardian: DynModel<Completion>,
}

impl ModelHandle {
    pub fn new(reference: ModelRef, kind: AccountKind, completion: DynModel<Completion>) -> Self {
        ModelHandle {
            reference,
            kind,
            completion,
            thinking_off: None,
            output_probe: None,
            quota: None,
            scopes: None,
            client: None,
            facts_notice: None,
        }
    }

    /// A handle on `reference`, completing through `client`.
    pub fn from_client(reference: ModelRef, kind: AccountKind, client: &Client) -> Self {
        let mut handle = Self::guardian_from_client(reference, kind, client);
        handle.completion = handle.conversation_completion();
        handle
    }

    fn conversation_completion(&self) -> DynModel<Completion> {
        self.scopes
            .as_ref()
            .map_or_else(|| self.completion.clone(), |s| s.conversation.clone())
    }

    /// This model with only the static prefix cacheable, for a guardian that falls back to the
    /// main model.
    pub fn guardian_scoped(&self) -> Self {
        let mut handle = self.clone();
        if let Some(scopes) = &self.scopes {
            handle.completion = scopes.guardian.clone();
        }
        handle
    }

    /// This model with its whole request cacheable, for a subagent that runs on it.
    pub fn conversation_scoped(&self) -> Self {
        let mut handle = self.clone();
        handle.completion = self.conversation_completion();
        handle
    }

    /// This model for the agent `namespace`, with its conversation completion and both cached
    /// scopes rebuilt through the client it came from. A kind that derives nothing from the
    /// namespace gives the same completions back.
    pub fn for_agent(&self, namespace: &str) -> Self {
        let (Some(client), Some(_)) = (&self.client, &self.scopes) else {
            return self.clone();
        };
        let model = &self.reference.model;
        let conversation = client.0.completion_for_agent(model, namespace);
        let mut handle = self.clone();
        handle.completion = conversation.clone();
        handle.scopes = Some(Scopes {
            conversation,
            guardian: client.0.guardian_completion_for_agent(model, namespace),
        });
        handle
    }

    /// A handle for the guardian: the same model, with only the static prefix cacheable.
    pub fn guardian_from_client(reference: ModelRef, kind: AccountKind, client: &Client) -> Self {
        let guardian = client.guardian_completion(&reference.model);
        let thinking_off = client.thinking_off(&reference.model);
        let output_probe = client.output_probe(&reference.model);
        let quota = client.quota_source();
        let conversation = client.completion(&reference.model);
        let facts_notice = client.facts_notice(&reference.model);
        ModelHandle {
            reference,
            kind,
            completion: guardian.clone(),
            thinking_off,
            output_probe,
            quota,
            scopes: Some(Scopes {
                conversation,
                guardian,
            }),
            client: Some(ClientRef(client.clone())),
            facts_notice,
        }
    }

    /// The provider's `/models` entry for this handle's model; `None` when the handle holds no
    /// client or the endpoint does not say; `Err` is why the lookup failed.
    pub fn served_model(
        &self,
    ) -> futures::future::BoxFuture<'_, Result<Option<ServedModel>, String>> {
        match &self.client {
            Some(client) => client.0.served_model(&self.reference.model),
            None => Box::pin(std::future::ready(Ok(None))),
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

    /// The generation options one turn on this model sends for `effort`. `None` sends nothing,
    /// so the model thinks at its own default. A level the model's catalog entry or its wire
    /// refuses (a gateway rig has no option mapping for refuses every level) is left out and
    /// returned as the refusal, because effort is a hint and a session must not fail on it; the
    /// caller tells the user.
    pub fn generation_options(
        &self,
        effort: Option<Effort>,
    ) -> (Option<GenerationOptions>, Option<UnsupportedOption>) {
        let Some(effort) = effort else {
            return (None, None);
        };
        let options = GenerationOptions::new().reasoning(effort.to_reasoning());
        if let Some(Err(refused)) = self.completion.spec().map(|spec| spec.validate(&options)) {
            return (None, Some(refused));
        }
        // Only the options are under test, so any other way the probe fails to build is not
        // this level's refusal.
        let probe = CompletionRequest::new("probe").options(options.clone());
        if let Err(CheckError::Unsupported(refused)) = self.completion.check(&probe)
            && let Some(first) = refused.into_iter().next()
        {
            return (None, Some(first));
        }
        (Some(options), None)
    }

    /// `generation_options`, telling the human once through `state` when the level is refused.
    pub fn generation_options_warned(
        &self,
        state: &SessionState,
        effort: Option<Effort>,
    ) -> Option<GenerationOptions> {
        let (options, refused) = self.generation_options(effort);
        if let (Some(effort), Some(refused)) = (effort, refused) {
            state.warn_effort_refused(&self.reference.to_string(), effort, &refused);
        }
        options
    }

    /// The levels this model takes. A wire with no option mapping takes none.
    pub fn supported_efforts(&self) -> Vec<Effort> {
        Effort::ALL
            .into_iter()
            .filter(|level| self.generation_options(Some(*level)).1.is_none())
            .collect()
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
    /// What the main model's account listed when it was resolved.
    pub main_listing: Listing,
    pub guardian: Option<GuardianModel>,
    pub guardian_choice: GuardianChoice,
    /// Things the user should be told once at startup.
    pub notices: Vec<crate::notice::Notice>,
}

pub struct ResolveParams<'a> {
    pub config: &'a Config,
    pub data_dir: &'a Path,
    pub env: SharedEnv,
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
    // The external program owns the login: nothing stored or named here may stand in for it.
    if account.kind.provider().credential() == CredentialPolicy::ExternalCli {
        return Credential::NoSource;
    }
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
    matches!(
        kind.provider().credential(),
        CredentialPolicy::Required | CredentialPolicy::SignIn
    )
}

fn missing_credential_hint(name: &str) -> String {
    format!("set `api_key_env` in [accounts.{name}] or run `/login {name}` in `fontaine`")
}

/// A client for `name`, or the reason none can be built: the credential is missing or a
/// configured source yields nothing.
fn client_for(
    name: &str,
    account: &Account,
    env: &SharedEnv,
    src: &Sources<'_>,
    network: &LoadedNetwork,
) -> Result<Client, String> {
    let provider = account.kind.provider();
    let credential = match credential(name, account, &**env, &src.stored) {
        Credential::Found(found) => Some(found),
        Credential::NoSource => match provider.credential() {
            CredentialPolicy::Optional | CredentialPolicy::ExternalCli => None,
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
        network,
        env: env.clone(),
        warn: crate::providers::warn::Warn::process(),
    })
}

/// The loaded network settings for one account, or why they cannot be loaded.
pub fn account_network(config: &Config, account: &str) -> Result<LoadedNetwork, String> {
    config.loaded_network(Some(account)).map_err(|e| {
        format!(
            "account `{account}`: {}",
            fontaine_tools::error_chain::chain(&e)
        )
    })
}

/// A transport error's own message rarely says why the request failed; the reason (refused,
/// reset, timed out) is further down the source chain. rig's `ProviderError::source` drops the
/// transport error, so the walk starts from it directly. Causes whose text is already shown are
/// skipped, since wrappers often repeat their source's message.
fn with_causes(error: &ProviderError) -> String {
    // The Claude CLI adapter's failures are already complete sentences for a person.
    if let Some(claude) = crate::providers::claude_subscription::claude_error(error) {
        return fontaine_tools::error_chain::chain(claude);
    }
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

/// The accounts the config names, or what to do when it names none.
pub fn known_accounts(config: &Config) -> String {
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

/// How long listing one account's models may take while a model is being resolved. The kinds that
/// discover through a local program need more than a picker's wait, but nothing is unbounded.
pub const RESOLVE_LISTING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(240);

/// What an account offers: the ids a model can be selected by, and the rows it listed but refuses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub models: Vec<String>,
    pub refused: Vec<RefusedModel>,
}

impl Listing {
    /// This listing narrowed to the ids in `allowed`.
    fn narrowed(self, allowed: &[String]) -> Listing {
        Listing {
            models: self
                .models
                .into_iter()
                .filter(|id| allowed.contains(id))
                .collect(),
            refused: self
                .refused
                .into_iter()
                .filter(|row| allowed.contains(&row.id))
                .collect(),
        }
    }
}

/// The ids `account` offers and the rows it refuses: its provider's listing. Bounded by
/// [`RESOLVE_LISTING_TIMEOUT`].
async fn list_models(client: &Client, name: &str) -> Result<Listing, ModelError> {
    match tokio::time::timeout(RESOLVE_LISTING_TIMEOUT, client.list()).await {
        Ok(Ok(models)) => Ok(Listing {
            models,
            refused: client.refused(),
        }),
        Ok(Err(e)) => Err(ModelError::Provider(format!(
            "cannot list the models of account `{name}`: {}",
            with_causes(&e)
        ))),
        Err(_) => Err(ModelError::Provider(format!(
            "listing the models of account `{name}` timed out after {RESOLVE_LISTING_TIMEOUT:?}"
        ))),
    }
}

/// The ids to check a requested model against. A configured `models` list stands in for a
/// listing, except for a kind whose models can only be built after a listing: there it narrows
/// the completed listing and never replaces it.
async fn models_of(client: &Client, name: &str, account: &Account) -> Result<Listing, ModelError> {
    match &account.models {
        Some(list) if !account.kind.provider().requires_discovery() => Ok(Listing {
            models: list.clone(),
            refused: Vec::new(),
        }),
        Some(allowed) => Ok(list_models(client, name).await?.narrowed(allowed)),
        None => list_models(client, name).await,
    }
}

/// The error for a model that is not in the checked list: the kind's own reason when it has one.
fn not_offered(client: &Client, reference: &ModelRef, available: &[String]) -> ModelError {
    match client.unavailable_reason(&reference.model) {
        Some(reason) => config_error(reason),
        None => unknown_model(reference, available),
    }
}

/// The main model's client, its listing and its handle.
async fn resolve_main_model(
    config: &Config,
    src: &Sources<'_>,
    env: &SharedEnv,
    text: &str,
    what: &str,
) -> Result<(ModelRef, Client, Listing, ModelHandle), ModelError> {
    let main_ref = parse_reference(text, what)?;
    let main_account = account_of(config, &main_ref, what)?;
    let main_client = {
        let network = account_network(config, &main_ref.account).map_err(config_error)?;
        client_for(&main_ref.account, main_account, env, src, &network).map_err(config_error)?
    };
    let main_models = models_of(&main_client, &main_ref.account, main_account).await?;
    if !lists(main_account.kind, &main_models.models, &main_ref.model) {
        return Err(not_offered(&main_client, &main_ref, &main_models.models));
    }
    let main = ModelHandle::from_client(main_ref.clone(), main_account.kind, &main_client);
    Ok((main_ref, main_client, main_models, main))
}

/// Resolves the main model and the guardian model.
///
/// An account without a `models` list is listed through the provider, lazily and only for the
/// main model's account. The guardian's model is checked against a list only when that list is
/// known without a call to the guardian's own credentials; its availability is otherwise the
/// guardian's startup concern and fails closed. A kind that needs its listing to build a model
/// is always listed, for the main model and the guardian alike. With no `[guardian] model`, the
/// default is the highest-ranked validated model an account offers, and the main model when none
/// does.
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
        resolve_main_model(config, &stored, &params.env, main_text, "model").await?;

    let mut notices = Vec::new();
    notices.extend(main.facts_notice.clone().map(facts_warning));
    let main_ids = main_models.models.as_slice();
    let (guardian, guardian_choice) = match config.guardian.model.as_deref() {
        Some(text) => (
            resolve_guardian(
                config,
                &stored,
                &params.env,
                text,
                (&main_ref, &main_client, main_ids),
            )
            .await?,
            GuardianChoice::Pinned,
        ),
        None => match default_guardian(
            validated,
            config,
            &stored,
            &params.env,
            (&main_ref, &main_client, main_ids),
        )
        .await
        {
            Some(guardian) => (guardian, GuardianChoice::Default),
            None => {
                notices.push(crate::notice::Notice::warning(
                    format!("Guardian is running on your main model `{main_ref}`"),
                    Some("No benchmark-validated guardian model is available on your accounts — set `[guardian] model` or run /guardian-model".into()),
                ));
                (
                    GuardianModel::Ready(main.guardian_scoped()),
                    GuardianChoice::MainFallback,
                )
            }
        },
    };
    if let GuardianModel::Ready(handle) = &guardian
        && handle.reference != main_ref
    {
        notices.extend(handle.facts_notice.clone().map(facts_warning));
    }
    Ok(Models {
        main,
        main_listing: main_models,
        guardian: Some(guardian),
        guardian_choice,
        notices,
    })
}

fn facts_warning(notice: String) -> crate::notice::Notice {
    crate::notice::Notice::warning(notice, None)
}

async fn resolve_guardian(
    config: &Config,
    stored: &Sources<'_>,
    env: &SharedEnv,
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
    .await
}

/// The guardian `text` names. The main model's account listing vouches for a guardian on that
/// account, and reuses the main client rather than building another. A kind that must be listed
/// to build a model is listed here whatever else is known, so its model facts exist.
async fn build_guardian(
    config: &Config,
    stored: &Sources<'_>,
    env: &SharedEnv,
    text: &str,
    main: Option<(&ModelRef, &[String])>,
    main_client: Option<&Client>,
) -> Result<GuardianModel, ModelError> {
    let reference = parse_reference(text, "guardian model")?;
    let account = account_of(config, &reference, "guardian model")?;
    let same_account = main.is_some_and(|(main_ref, _)| reference.account == main_ref.account);
    let discovers = account.kind.provider().requires_discovery();

    let known: Option<&[String]> = match (&account.models, main) {
        (Some(list), _) if !discovers => Some(list),
        (None, Some((_, listing))) if same_account => Some(listing),
        // The main account's listing is a completed discovery narrowed by `models`.
        (Some(_), Some((_, listing))) if same_account && main_client.is_some() => Some(listing),
        _ => None,
    };
    if let Some(list) = known
        && !lists(account.kind, list, &reference.model)
    {
        return Err(match main_client.filter(|_| same_account) {
            Some(client) => not_offered(client, &reference, list),
            None => unknown_model(&reference, list),
        });
    }

    let reused = main_client.filter(|_| same_account && (known.is_some() || !discovers));
    let client = match reused {
        Some(client) => Ok(client.clone()),
        None => account_network(config, &reference.account)
            .and_then(|network| client_for(&reference.account, account, env, stored, &network)),
    };
    let client = match client {
        Ok(client) if discovers && reused.is_none() => {
            match models_of(&client, &reference.account, account).await {
                Ok(listing) if lists(account.kind, &listing.models, &reference.model) => Ok(client),
                Ok(listing) => {
                    return Err(not_offered(&client, &reference, &listing.models));
                }
                Err(e) => Err(e.to_string()),
            }
        }
        other => other,
    };
    Ok(match client {
        Ok(client) => GuardianModel::Ready(ModelHandle::guardian_from_client(
            reference,
            account.kind,
            &client,
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
    // Its listing is how a discovering kind says what it offers, so only a configured list can
    // rule it out here; the guardian's resolution lists it.
    if account.kind.provider().requires_discovery() {
        return account
            .models
            .as_ref()
            .is_none_or(|list| lists(account.kind, list, model));
    }
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

async fn default_guardian(
    validated: &[ModelEntry],
    config: &Config,
    stored: &Sources<'_>,
    env: &SharedEnv,
    main: (&ModelRef, &Client, &[String]),
) -> Option<GuardianModel> {
    for reference in available_validated(validated, config, stored, &**env, (main.0, main.2)) {
        let text = reference.to_string();
        if let Ok(ready @ GuardianModel::Ready(_)) =
            resolve_guardian(config, stored, env, &text, main).await
        {
            return Some(ready);
        }
    }
    None
}

/// How long one account may take to list its models before the picker shows it as failed.
pub const LISTING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// One configured account's models, or why they could not be listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountListing {
    pub account: String,
    pub kind: AccountKind,
    pub models: Result<Vec<String>, String>,
    /// The rows the account listed but refuses; empty when `models` is an error.
    pub refused: Vec<RefusedModel>,
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
    pub async fn main(&self, text: &str) -> Result<(ModelHandle, Listing), ModelError> {
        let stored = self.sources()?;
        let (_, _, listing, handle) =
            resolve_main_model(&self.config, &stored, &self.env, text, "model").await?;
        Ok((handle, listing))
    }

    /// The guardian model `text` names. An account without a credential gives an unavailable
    /// guardian, not an error: it fails closed and the user is asked what to do. `main` is the
    /// main model and the listing of its account, which vouches for a guardian on that account.
    pub async fn guardian(
        &self,
        text: &str,
        main: Option<(&ModelRef, &[String])>,
    ) -> Result<GuardianModel, ModelError> {
        let stored = self.sources()?;
        build_guardian(&self.config, &stored, &self.env, text, main, None).await
    }

    /// The model `$guardian_default` stands for: the top-ranked validated guardian model an
    /// account offers, or `None` when none is available.
    pub async fn guardian_default(
        &self,
        main: &ModelRef,
        main_models: &[String],
    ) -> Option<ModelHandle> {
        let stored = self.sources().ok()?;
        let candidates = available_validated(
            &validated_models(),
            &self.config,
            &stored,
            &*self.env,
            (main, main_models),
        );
        for reference in candidates {
            if let Ok(GuardianModel::Ready(handle)) = build_guardian(
                &self.config,
                &stored,
                &self.env,
                &reference.to_string(),
                Some((main, main_models)),
                None,
            )
            .await
            {
                return Some(handle);
            }
        }
        None
    }

    /// The models of every account, or only of those named in `only`, sorted by account name.
    /// An explicit `models` list and the main account's already-fetched `main` listing (none
    /// before a session has a main model) cost no call; the others are listed concurrently. A
    /// failure keeps the account's entry.
    pub async fn listings(
        &self,
        main: Option<(&ModelRef, &Listing)>,
        only: Option<&[String]>,
    ) -> Vec<AccountListing> {
        self.listings_within(main, only, LISTING_TIMEOUT).await
    }

    /// Tests only: [`Switcher::listings`] with its own per-account timeout.
    pub async fn listings_within(
        &self,
        main: Option<(&ModelRef, &Listing)>,
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
                        refused: Vec::new(),
                    })
                    .collect();
            }
        };
        let stored = &stored;
        futures::future::join_all(wanted.map(|(name, account)| async move {
            let discovers = account.kind.provider().requires_discovery();
            let listed: Result<Listing, String> =
                if let Some(list) = account.models.as_ref().filter(|_| !discovers) {
                    Ok(Listing {
                        models: list.clone(),
                        refused: Vec::new(),
                    })
                } else if let Some(main) = main.filter(|main| *name == main.0.account) {
                    Ok(main.1.clone())
                } else {
                    let client = account_network(&self.config, name)
                        .and_then(|network| client_for(name, account, &self.env, stored, &network));
                    match client {
                        Err(reason) => Err(reason),
                        Ok(client) => match tokio::time::timeout(timeout, client.list()).await {
                            Ok(Ok(models)) => {
                                let listing = Listing {
                                    models,
                                    refused: client.refused(),
                                };
                                Ok(match &account.models {
                                    Some(allowed) => listing.narrowed(allowed),
                                    None => listing,
                                })
                            }
                            Ok(Err(e)) => Err(with_causes(&e)),
                            Err(_) => Err(format!("listing timed out after {timeout:?}")),
                        },
                    }
                };
            let (models, refused) = match listed {
                Ok(listing) => (Ok(listing.models), listing.refused),
                Err(reason) => (Err(reason), Vec::new()),
            };
            AccountListing {
                account: name.clone(),
                kind: account.kind,
                models,
                refused,
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

/// The check `/login` runs for an account whose login an external program owns: connect the
/// kind, list its models (which makes the program's own checks) and summarize.
pub(crate) async fn external_check(
    name: &str,
    account: &Account,
    data_dir: &Path,
    network: &LoadedNetwork,
    env: SharedEnv,
) -> Result<String, String> {
    let stored = Credentials::default();
    let src = Sources {
        stored,
        data_dir,
        session_id: None,
    };
    let client = client_for(name, account, &env, &src, network)?;
    let models = models_of(&client, name, account)
        .await
        .map_err(|e| fontaine_tools::error_chain::chain(&e))?;
    Ok(format!(
        "`{name}` is ready: {} model(s) available through the external login",
        models.models.len()
    ))
}
