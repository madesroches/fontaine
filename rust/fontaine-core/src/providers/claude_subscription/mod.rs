//! `claude-subscription`: the user's installed, unmodified Claude Code CLI as a model-only
//! transport. The CLI owns the Claude login and its subscription billing; Fontaine keeps its own
//! agent loop, guardian, tools and sessions. Requests are rig's Anthropic Messages wire with
//! its HTTP transport replaced by one that runs a CLI process per request and reads the model's
//! reply from the process's stream-json output.
//!
//! Fontaine never reads or forwards a Claude credential: the CLI is launched with a rebuilt
//! environment and only ever authenticates itself.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use futures::FutureExt;
use futures::future::BoxFuture;
use rig_core::DynModel;
use rig_core::catalog::{Catalog, ModelFacts};
use rig_core::driver::Model;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::anthropic::AnthropicConfig;
use rig_core::providers::registry::ProviderId;

use super::{AccountClient, Billing, ConnectContext, CredentialPolicy, Provider};
use crate::config::Account;
use fontaine_tools::lock::{read, write};

pub mod cli;
pub mod inventory;
pub mod protocol;
pub mod transport;

pub use cli::{CONFLICTING_ENV, Cli, MIN_VERSION, Timeouts, inherited_env_names};
pub use inventory::INVENTORY_FLAG;
pub use transport::{ClaudeTransport, InventoryCommand};

/// Where the account's own extra-usage setting is changed.
pub const USAGE_SETTINGS_URL: &str = "https://claude.ai/settings/usage";

/// The namespace the root agent's requests derive their CLI session id from.
pub const ROOT_NAMESPACE: &str = "root";
/// The namespace of the guardian's requests.
pub const GUARDIAN_NAMESPACE: &str = "guardian";

/// What kind of failure a CLI problem is. None of them is worth a retry: each needs the user to
/// change something, or is a bug the next attempt would repeat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The program cannot be started, or a probe of it failed.
    Cli,
    /// The version is missing, malformed, too old or its probe timed out.
    Version,
    /// Not signed in, or the sign-in cannot be used.
    Auth,
    /// Signed in with a method or plan this adapter does not accept.
    Plan,
    /// An inherited variable would change which backend or account bills.
    Environment,
    /// The model draws from paid usage credits.
    UsageCredits,
    /// The CLI's output is not the protocol this adapter qualified.
    Protocol,
    /// The request or its history cannot be represented without loss.
    Unsupported,
    /// A model without the facts the request needs.
    Catalog,
    /// The CLI reported a plan limit, allowance or billing failure.
    Limit,
    /// An auth or discovery process produced nothing in time.
    SetupTimeout,
}

/// A CLI failure with a message that is safe to show and to export: fixed text or text built
/// from names, never raw CLI output or a credential value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeError {
    pub kind: ErrorKind,
    message: String,
}

impl ClaudeError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        ClaudeError {
            kind,
            message: message.into(),
        }
    }

    /// The failure as a non-retryable provider error that keeps the typed error as its source.
    pub fn provider(self) -> ProviderError {
        ProviderError::request(self)
    }

    pub fn boxed(self) -> Box<ProviderError> {
        Box::new(self.provider())
    }
}

impl std::fmt::Display for ClaudeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ClaudeError {}

/// The typed error inside a provider error this adapter made.
pub fn claude_error(error: &ProviderError) -> Option<&ClaudeError> {
    match error {
        ProviderError::Request(inner) => inner.downcast_ref::<ClaudeError>(),
        _ => None,
    }
}

/// A read-idle expiry of a model process: a retryable transport timeout.
#[derive(Debug)]
pub(crate) struct ReadIdle(pub std::time::Duration);

impl std::fmt::Display for ReadIdle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the Claude CLI sent no output for {} seconds",
            self.0.as_secs()
        )
    }
}

impl std::error::Error for ReadIdle {}

pub(crate) fn read_idle_error(idle: std::time::Duration) -> ProviderError {
    ProviderError::Http(Arc::new(rig_core::http_client::Error::Instance(Box::new(
        ReadIdle(idle),
    ))))
}

pub struct ClaudeSubscription;

const NOTICE: &str = "Requests go through your installed Claude Code CLI and draw from your \
    Claude subscription limits; token usage is shown but never priced. If your account has extra \
    usage enabled, exhausting the included limit can switch it to paid usage credits, which \
    Fontaine cannot see; disable it at https://claude.ai/settings/usage if you need \
    included-subscription-only use.";

impl Provider for ClaudeSubscription {
    fn name(&self) -> &'static str {
        "claude-subscription"
    }

    fn credential(&self) -> CredentialPolicy {
        CredentialPolicy::ExternalCli
    }

    fn billing(&self) -> Billing {
        Billing::Unpriced
    }

    fn requires_discovery(&self) -> bool {
        true
    }

    fn selection_notice(&self) -> Option<&'static str> {
        Some(NOTICE)
    }

    fn validate(&self, account: &Account) -> Result<(), (&'static str, String)> {
        let refuse = |key: &'static str, why: &str| Err((key, why.to_string()));
        if account.api_key_env.is_some() {
            return refuse(
                "api_key_env",
                "`claude-subscription` accounts use the Claude CLI's own login; `api_key_env` is not used",
            );
        }
        if account.base_url.is_some() {
            return refuse("base_url", "`claude-subscription` has no endpoint to set");
        }
        for (key, set) in [
            ("context_tokens", account.context_tokens.is_some()),
            ("think", account.think.is_some()),
        ] {
            if set {
                return refuse(key, "not used by kind = \"claude-subscription\"");
            }
        }
        for (key, set) in [
            ("https_proxy", account.https_proxy.is_some()),
            ("http_proxy", account.http_proxy.is_some()),
            ("no_proxy", account.no_proxy.is_some()),
            ("ca_bundle", account.ca_bundle.is_some()),
            ("client_cert", account.client_cert.is_some()),
            ("client_key", account.client_key.is_some()),
        ] {
            if set {
                return refuse(
                    key,
                    "the Claude CLI reads its own proxy and TLS environment; set it there",
                );
            }
        }
        for (key, path) in [
            ("cli_path", &account.cli_path),
            ("claude_config_dir", &account.claude_config_dir),
        ] {
            if let Some(path) = path
                && !path.is_absolute()
            {
                return refuse(key, "must be an absolute path");
            }
        }
        Ok(())
    }

    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String> {
        let timeouts = Timeouts::from_env(&*ctx.env);
        let cli = Cli::new(
            ctx.account.cli_path.clone(),
            ctx.account.claude_config_dir.clone(),
            ctx.env.clone(),
            timeouts,
        );
        let session = ctx
            .session_id
            .map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_string);
        Ok(Arc::new(Connected {
            cli: Arc::new(cli),
            session,
            inventory: InventoryCommand::current_exe(),
            discovered: RwLock::new(BTreeMap::new()),
        }))
    }
}

/// What discovery learned about one listed model.
#[derive(Clone, Debug)]
pub(crate) enum Entry {
    Usable(Route),
    /// Listed but not usable, with the reason a selection is refused for.
    Refused(String),
}

/// A model the CLI offers: the alias the user selects, the canonical id both rig and the CLI
/// receive, and rig's facts for it.
#[derive(Clone, Debug)]
pub struct Route {
    pub alias: String,
    pub canonical: String,
    pub facts: ModelFacts,
}

struct Connected {
    cli: Arc<Cli>,
    session: String,
    inventory: InventoryCommand,
    discovered: RwLock<BTreeMap<String, Entry>>,
}

impl Connected {
    fn model(&self, model: &str, namespace: &str) -> DynModel<Completion> {
        let entry = read(&self.discovered, "claude model catalog")
            .get(model)
            .cloned();
        let (route, refused) = match entry {
            Some(Entry::Usable(route)) => (Some(route), None),
            Some(Entry::Refused(reason)) => (None, Some(reason)),
            None => (
                None,
                Some(format!(
                    "the Claude CLI's model `{model}` was not discovered; its models are listed before it is used"
                )),
            ),
        };
        let canonical = route
            .as_ref()
            .map_or_else(|| model.to_string(), |r| r.canonical.clone());
        let mut wire = AnthropicConfig::new("").completion(canonical.clone());
        if let Some(route) = &route {
            wire = wire.with_facts(route.facts.clone());
        }
        let transport = ClaudeTransport::new(
            self.cli.clone(),
            transport::Selection {
                alias: model.to_string(),
                canonical,
                session: self.session.clone(),
                namespace: namespace.to_string(),
                refused: refused.map(|reason| ClaudeError::new(ErrorKind::Catalog, reason)),
            },
            self.inventory.clone(),
        );
        Model::new(wire, transport).erase()
    }
}

impl AccountClient for Connected {
    fn completion(&self, model: &str) -> DynModel<Completion> {
        self.model(model, ROOT_NAMESPACE)
    }

    fn guardian_completion(&self, model: &str) -> DynModel<Completion> {
        self.model(model, GUARDIAN_NAMESPACE)
    }

    fn completion_for_agent(&self, model: &str, namespace: &str) -> DynModel<Completion> {
        self.model(model, namespace)
    }

    fn guardian_completion_for_agent(&self, model: &str, namespace: &str) -> DynModel<Completion> {
        self.model(model, namespace)
    }

    fn unavailable_reason(&self, model: &str) -> Option<String> {
        match read(&self.discovered, "claude model catalog").get(model) {
            Some(Entry::Refused(reason)) => Some(reason.clone()),
            _ => None,
        }
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>> {
        async move {
            let rows = self.cli.discover().await.map_err(ClaudeError::boxed)?;
            let mut entries = BTreeMap::new();
            let mut usable = Vec::new();
            for row in rows {
                if entries.contains_key(&row.id) {
                    continue;
                }
                let entry = entry_of(&row);
                if matches!(entry, Entry::Usable(_)) {
                    usable.push(row.id.clone());
                }
                entries.insert(row.id, entry);
            }
            *write(&self.discovered, "claude model catalog") = entries;
            Ok(usable)
        }
        .boxed()
    }
}

/// The facts for a discovered row, or why the row is refused.
fn entry_of(row: &cli::ModelRow) -> Entry {
    if row.credits {
        return Entry::Refused(format!(
            "the Claude CLI marks model `{}` as drawing from paid usage credits, which this \
             adapter never selects; choose another model or see {USAGE_SETTINGS_URL}",
            row.id
        ));
    }
    let Some(vendor) = ProviderId::catalog("anthropic") else {
        return Entry::Refused("rig has no Anthropic catalog".to_string());
    };
    let facts = Catalog::builtin().facts(vendor, &row.canonical);
    let usable = facts
        .spec()
        .is_some_and(|spec| spec.max_output_tokens.is_some());
    if !usable {
        return Entry::Refused(format!(
            "model `{}` resolves to `{}`, which this build of Fontaine has no model facts or \
             default output limit for; update Fontaine",
            row.id, row.canonical
        ));
    }
    Entry::Usable(Route {
        alias: row.id.clone(),
        canonical: row.canonical.clone(),
        facts,
    })
}
