//! Account operations behind the provider dialog: listing accounts with their sign-in state,
//! adding one to the user settings file, storing or removing a credential, signing in, and
//! removing an account. Nothing here prints; progress goes to a caller-given sink.

use std::fmt;
use std::path::Path;

use crate::config::{Account, AccountKind, Config, ConfigError, EnvLookup, check_account};
use crate::credentials::{self, Credentials, StoredCredential};
use crate::models::{Credential, credential};
use crate::providers::{self, CredentialPolicy, SignInError, SignInProgress};
use crate::user_config;

pub use crate::providers::{BaseUrlPolicy, SignInStep};

/// The facts a new account needs; everything else keeps its default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewAccount {
    pub name: String,
    pub kind: AccountKind,
    pub base_url: Option<String>,
}

/// What an account has to authenticate with, for the dialog's list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignInState {
    /// A sign-in is stored; `detail` is the email it was made with.
    SignedIn {
        detail: Option<String>,
    },
    KeyStored,
    /// `api_key_env` is set and the variable is non-empty.
    KeyFromEnv {
        var: String,
    },
    /// `api_key_env` is set and the variable is unset or empty.
    EnvUnset {
        var: String,
    },
    /// A key or sign-in is required and nothing usable is stored.
    NotSignedIn,
    /// A key is optional and none is stored.
    NoKeyNeeded,
}

impl fmt::Display for SignInState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SignInState::SignedIn { detail: Some(who) } => write!(f, "signed in ({who})"),
            SignInState::SignedIn { detail: None } => f.write_str("signed in"),
            SignInState::KeyStored => f.write_str("key stored"),
            SignInState::KeyFromEnv { var } => write!(f, "key from ${var}"),
            SignInState::EnvUnset { var } => write!(f, "${var} is not set"),
            SignInState::NotSignedIn => f.write_str("not signed in"),
            SignInState::NoKeyNeeded => f.write_str("no key needed"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountRow {
    pub name: String,
    pub kind: AccountKind,
    /// The account's `base_url`, else the kind's own endpoint, else "default".
    pub endpoint: String,
    pub state: SignInState,
}

/// Why an account change was refused or failed. Only the kinds the dialog tells apart are
/// variants; everything else is an I/O failure with its cause kept as the source.
#[derive(Debug)]
pub enum AccountsError {
    /// The live session or the user settings file uses the account, as `by` says ("the main
    /// model", "`[guardian] model`").
    InUse {
        by: &'static str,
    },
    /// The account is defined in another settings file than the user's own.
    NotInUserFile,
    Invalid(String),
    /// The account is gone from the settings, but its stored credential could not be removed.
    CredentialKept {
        account: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    Io {
        context: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl fmt::Display for AccountsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccountsError::InUse { by } => write!(f, "the account is in use by {by}"),
            AccountsError::NotInUserFile => {
                f.write_str("the account is defined in another settings file than your own")
            }
            AccountsError::Invalid(message) => f.write_str(message),
            AccountsError::CredentialKept { account, .. } => write!(
                f,
                "`{account}` was removed, but its stored credential could not be"
            ),
            AccountsError::Io { context, .. } => f.write_str(context),
        }
    }
}

impl std::error::Error for AccountsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AccountsError::Io { source, .. } | AccountsError::CredentialKept { source, .. } => {
                Some(source.as_ref())
            }
            _ => None,
        }
    }
}

fn io_error(
    context: impl Into<String>,
    source: impl std::error::Error + Send + Sync + 'static,
) -> AccountsError {
    AccountsError::Io {
        context: context.into(),
        source: Box::new(source),
    }
}

/// Every configured account with what it authenticates with, sorted by name. It asks
/// [`credential`] so the list says exactly what resolving the account would do.
pub fn rows(
    config: &Config,
    data_dir: &Path,
    env: EnvLookup<'_>,
) -> Result<Vec<AccountRow>, ConfigError> {
    let stored = Credentials::load(data_dir)?;
    Ok(config
        .accounts
        .iter()
        .map(|(name, account)| {
            let policy = account.kind.provider().credential();
            let state = match credential(name, account, env, &stored) {
                Credential::Found(StoredCredential::OAuth(tokens)) => SignInState::SignedIn {
                    detail: tokens.extra.get("email").cloned(),
                },
                Credential::Found(StoredCredential::ApiKey(_)) => match &account.api_key_env {
                    Some(var) => SignInState::KeyFromEnv { var: var.clone() },
                    None => SignInState::KeyStored,
                },
                Credential::Unresolved(_) => SignInState::EnvUnset {
                    var: account.api_key_env.clone().unwrap_or_default(),
                },
                Credential::NoSource if policy == CredentialPolicy::Optional => {
                    SignInState::NoKeyNeeded
                }
                Credential::NoSource => SignInState::NotSignedIn,
            };
            AccountRow {
                name: name.clone(),
                kind: account.kind,
                endpoint: account
                    .base_url
                    .as_deref()
                    .filter(|url| !url.is_empty())
                    .or_else(|| account.kind.provider().default_endpoint())
                    .unwrap_or("default")
                    .to_string(),
                state,
            }
        })
        .collect())
}

/// Whether `name` can be given to a new account. Stricter than what a hand-written file may
/// use, so a dialog-made name is always addressable as `/login <name>` and `<account>/<model>`.
pub fn check_name(name: &str, config: &Config) -> Result<(), String> {
    check_name_in(name, &|taken| config.accounts.contains_key(taken))
}

/// [`check_name`] for a caller that knows the existing names but holds no [`Config`].
pub fn check_name_in(name: &str, exists: &dyn Fn(&str) -> bool) -> Result<(), String> {
    if name.is_empty() {
        return Err("a name is required".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("use only letters, digits, `-` and `_`".to_string());
    }
    if exists(name) {
        return Err(format!("an account named `{name}` already exists"));
    }
    Ok(())
}

fn check_base_url(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("`{url}` is not a URL: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(format!("`{url}` is not an http(s) URL with a host"));
    }
    Ok(())
}

/// The account `new` describes, after the checks the settings loader would make on it.
pub fn build(new: &NewAccount, config: &Config) -> Result<Account, String> {
    check_name(&new.name, config)?;
    let base_url = new
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty());
    if let Some(url) = base_url {
        check_base_url(url)?;
    }
    let account = Account {
        kind: new.kind,
        api_key_env: None,
        base_url: base_url.map(str::to_string),
        models: None,
        wires: None,
    };
    check_account(&new.name, &account).map_err(|(key, message)| format!("{key}: {message}"))?;
    Ok(account)
}

/// Writes the new account to the user settings file `user_file` and returns it for the caller to
/// install in the live configuration. A credential already stored under the name is deleted
/// first, so one left behind by a removed account is never sent to the new account's endpoint.
pub fn add(
    user_file: &Path,
    data_dir: &Path,
    new: &NewAccount,
    config: &Config,
) -> Result<Account, AccountsError> {
    let account = build(new, config).map_err(AccountsError::Invalid)?;
    credentials::remove(data_dir, &new.name).map_err(|e| {
        io_error(
            format!("could not clear the stored credential for `{}`", new.name),
            e,
        )
    })?;
    user_config::add_account(user_file, &new.name, &account).map_err(|e| {
        io_error(
            format!("could not add `{}` to {}", new.name, user_file.display()),
            e,
        )
    })?;
    Ok(account)
}

/// Stores `key` as account `name`'s API key.
pub fn store_key(data_dir: &Path, name: &str, key: &str) -> Result<(), ConfigError> {
    credentials::store(data_dir, name, key)
}

/// Runs the account kind's interactive sign-in and returns its summary line.
pub async fn sign_in(
    name: &str,
    account: &Account,
    data_dir: &Path,
    progress: SignInProgress<'_>,
) -> Result<String, SignInError> {
    let Some(sign_in) = account.kind.provider().sign_in() else {
        return Err(SignInError(Box::new(std::io::Error::other(format!(
            "kind `{}` has no sign-in; store a key instead",
            account.kind.as_str()
        )))));
    };
    sign_in.sign_in(name, account, data_dir, progress).await
}

/// Removes the account's stored credential. The provider-side token is not revoked. `false` when
/// nothing was stored.
pub fn log_out(data_dir: &Path, name: &str) -> Result<bool, ConfigError> {
    credentials::remove(data_dir, name)
}

/// Refuses a removal of an account another scope, such as a `--settings` overlay, also defines:
/// that definition would outlive the removal and win again on the next start.
pub fn check_defined_only_in_user_file(config: &Config, name: &str) -> Result<(), AccountsError> {
    if config.shadowed_accounts.contains(name) {
        return Err(AccountsError::NotInUserFile);
    }
    Ok(())
}

/// Deletes `[accounts.<name>]` from the user settings file and then the account's stored
/// credential, so no orphaned secret is left. It has no view of the live session: a host that
/// has one refuses a removal of an account in use before calling this. A model setting in the
/// user file that names the account also refuses it, so the file is never left dangling.
pub fn remove(user_file: &Path, data_dir: &Path, name: &str) -> Result<(), AccountsError> {
    let key = user_config::model_key_naming(user_file, name)
        .map_err(|e| io_error(format!("could not read {}", user_file.display()), e))?;
    if let Some(key) = key {
        return Err(AccountsError::InUse { by: key });
    }
    let removed = user_config::remove_account(user_file, name).map_err(|e| {
        io_error(
            format!("could not remove `{name}` from {}", user_file.display()),
            e,
        )
    })?;
    if !removed {
        return Err(AccountsError::NotInUserFile);
    }
    credentials::remove(data_dir, name).map_err(|e| AccountsError::CredentialKept {
        account: name.to_string(),
        source: Box::new(e),
    })?;
    Ok(())
}

/// The kinds the dialog offers, in registry order.
pub fn kinds() -> Vec<AccountKind> {
    providers::names().map(AccountKind::named).collect()
}
