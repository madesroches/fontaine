//! A signed-in account's access token, kept fresh. One `TokenSource` per account serves every
//! client of that account in the process, and the refresh is coordinated across processes
//! through `credentials.lock`, because refresh tokens rotate: two unsynchronized refreshes would
//! leave one of them holding a dead token.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use futures::future::BoxFuture;

use super::device_flow::{FlowError, RefreshError};
use crate::config::ConfigError;
use crate::credentials::{self, Credentials, OAuthTokens, StoredCredential};

/// A token with less than this left is refreshed before use.
const REFRESH_MARGIN_SECS: u64 = 5 * 60;

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Exchanges a refresh token for a new pair.
pub trait Refresher: Send + Sync {
    fn refresh<'a>(
        &'a self,
        current: &'a OAuthTokens,
    ) -> BoxFuture<'a, Result<OAuthTokens, RefreshError>>;
}

/// Why no access token could be produced. Every case names the sign-in command, so the message
/// reaching the user already says what to do.
#[derive(Debug)]
pub struct TokenError {
    account: String,
    kind: Box<TokenErrorKind>,
}

#[derive(Debug)]
enum TokenErrorKind {
    SignedOut,
    NoSignIn,
    Refresh(FlowError),
    Store(ConfigError),
    Lock,
}

impl TokenError {
    fn new(account: &str, kind: TokenErrorKind) -> Self {
        TokenError {
            account: account.to_string(),
            kind: Box::new(kind),
        }
    }

    pub fn is_signed_out(&self) -> bool {
        matches!(
            *self.kind,
            TokenErrorKind::SignedOut | TokenErrorKind::NoSignIn
        )
    }
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let account = &self.account;
        match &*self.kind {
            TokenErrorKind::SignedOut => write!(
                f,
                "the sign-in for account `{account}` has ended; run `fontaine auth login {account}`"
            ),
            TokenErrorKind::NoSignIn => write!(
                f,
                "account `{account}` is not signed in; run `fontaine auth login {account}`"
            ),
            TokenErrorKind::Refresh(_) => write!(
                f,
                "could not refresh the sign-in for account `{account}`; if it persists, run `fontaine auth login {account}`"
            ),
            TokenErrorKind::Store(_) => write!(
                f,
                "could not update the stored sign-in for account `{account}`"
            ),
            TokenErrorKind::Lock => write!(
                f,
                "could not lock the credentials file to refresh the sign-in for account `{account}`"
            ),
        }
    }
}

impl std::error::Error for TokenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &*self.kind {
            TokenErrorKind::Refresh(e) => Some(e),
            TokenErrorKind::Store(e) => Some(e),
            _ => None,
        }
    }
}

pub struct TokenSource {
    data_dir: PathBuf,
    account: String,
    refresher: Arc<dyn Refresher>,
    cached: Mutex<OAuthTokens>,
    /// Single flight: callers that find the token stale queue here, and the first one refreshes.
    refreshing: tokio::sync::Mutex<()>,
}

type Registry = Mutex<HashMap<(PathBuf, String), Arc<TokenSource>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

impl TokenSource {
    pub fn new(
        data_dir: &Path,
        account: &str,
        tokens: OAuthTokens,
        refresher: Arc<dyn Refresher>,
    ) -> Arc<TokenSource> {
        Arc::new(TokenSource {
            data_dir: data_dir.to_path_buf(),
            account: account.to_string(),
            refresher,
            cached: Mutex::new(tokens),
            refreshing: tokio::sync::Mutex::new(()),
        })
    }

    /// The process-wide source for `account`, so clients rebuilt by a model switch and the
    /// guardian share one refresh. Tokens offered later replace the cached pair only when they
    /// are newer.
    pub fn shared(
        data_dir: &Path,
        account: &str,
        tokens: OAuthTokens,
        refresher: Arc<dyn Refresher>,
    ) -> Arc<TokenSource> {
        let mut sources = registry()
            .lock()
            .expect("the registry lock is not poisoned");
        let key = (data_dir.to_path_buf(), account.to_string());
        if let Some(existing) = sources.get(&key) {
            existing.offer(tokens);
            return existing.clone();
        }
        let source = TokenSource::new(data_dir, account, tokens, refresher);
        sources.insert(key, source.clone());
        source
    }

    fn offer(&self, tokens: OAuthTokens) {
        let mut cached = self.cached.lock().expect("the token lock is not poisoned");
        if tokens.refresh != cached.refresh && tokens.expires_at >= cached.expires_at {
            *cached = tokens;
        }
    }

    fn fresh(&self) -> Option<String> {
        let cached = self.cached.lock().expect("the token lock is not poisoned");
        (cached.expires_at > now_unix() + REFRESH_MARGIN_SECS).then(|| cached.access.clone())
    }

    /// An access token with more than the refresh margin left.
    pub async fn token(&self) -> Result<String, TokenError> {
        if let Some(token) = self.fresh() {
            return Ok(token);
        }
        let _turn = self.refreshing.lock().await;
        if let Some(token) = self.fresh() {
            return Ok(token);
        }
        let account = self.account.clone();
        let data_dir = self.data_dir.clone();
        let lock = tokio::task::spawn_blocking(move || credentials::lock(&data_dir))
            .await
            .map_err(|_| TokenError::new(&account, TokenErrorKind::Lock))?
            .map_err(|_| TokenError::new(&account, TokenErrorKind::Lock))?;

        // Another process may have rotated the pair since this one read it.
        let stored = Credentials::load(&self.data_dir)
            .map_err(|e| TokenError::new(&self.account, TokenErrorKind::Store(e)))?;
        let current = match stored.account(&self.account) {
            Some(StoredCredential::OAuth(tokens)) => tokens,
            _ => return Err(TokenError::new(&self.account, TokenErrorKind::NoSignIn)),
        };
        if current.expires_at > now_unix() + REFRESH_MARGIN_SECS {
            let access = current.access.clone();
            *self.cached.lock().expect("the token lock is not poisoned") = current;
            return Ok(access);
        }

        let refreshed = match self.refresher.refresh(&current).await {
            Ok(tokens) => tokens,
            Err(RefreshError::SignedOut) => {
                return Err(TokenError::new(&self.account, TokenErrorKind::SignedOut));
            }
            Err(RefreshError::Failed(e)) => {
                return Err(TokenError::new(&self.account, TokenErrorKind::Refresh(e)));
            }
        };
        credentials::store_oauth_locked(&self.data_dir, &self.account, &refreshed, &lock)
            .map_err(|e| TokenError::new(&self.account, TokenErrorKind::Store(e)))?;
        let access = refreshed.access.clone();
        *self.cached.lock().expect("the token lock is not poisoned") = refreshed;
        Ok(access)
    }
}
