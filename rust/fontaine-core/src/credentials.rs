use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::ConfigError;

pub const CREDENTIALS_FILE: &str = "credentials.toml";

pub fn credentials_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CREDENTIALS_FILE)
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    accounts: BTreeMap<String, Entry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    telemetry: Option<Entry>,
}

/// Exactly one of the two is set; [`check`] enforces it after parsing.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    oauth: Option<OAuthEntry>,
}

/// `extra` is flattened so provider metadata sits beside the tokens in the file. Unknown keys
/// are therefore kept rather than refused.
#[derive(Serialize, Deserialize)]
struct OAuthEntry {
    access: String,
    refresh: String,
    expires_at: u64,
    #[serde(flatten)]
    extra: BTreeMap<String, String>,
}

/// A sign-in's token pair. Not `Debug`-derived: the values are secrets.
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthTokens {
    pub access: String,
    pub refresh: String,
    /// Unix seconds.
    pub expires_at: u64,
    /// Provider metadata such as `server`, `org_id` and `email`.
    pub extra: BTreeMap<String, String>,
}

impl std::fmt::Debug for OAuthTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OAuthTokens(..)")
    }
}

impl From<&OAuthTokens> for OAuthEntry {
    fn from(t: &OAuthTokens) -> Self {
        OAuthEntry {
            access: t.access.clone(),
            refresh: t.refresh.clone(),
            expires_at: t.expires_at,
            extra: t.extra.clone(),
        }
    }
}

/// What an account has stored.
#[derive(Clone, PartialEq, Eq)]
pub enum StoredCredential {
    ApiKey(String),
    OAuth(OAuthTokens),
}

impl std::fmt::Debug for StoredCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoredCredential::ApiKey(_) => f.write_str("ApiKey(..)"),
            StoredCredential::OAuth(_) => f.write_str("OAuth(..)"),
        }
    }
}

/// `toml`'s message echoes wrongly typed values and unknown field names, either of which can be
/// a pasted key, so no text from the error is ever reported.
const MALFORMED: &str = "malformed credentials file";

/// Stored API keys, read-only. Not `Debug`: the values are secrets.
#[derive(Default)]
pub struct Credentials(File);

impl Credentials {
    /// A missing file is an empty store; any other I/O error or an unparseable file is a
    /// config error naming the path.
    pub fn load(data_dir: &Path) -> Result<Self, ConfigError> {
        let path = credentials_path(data_dir);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(ConfigError::Read { path, source }),
        };
        parse(&text, path).map(Credentials)
    }

    pub fn account(&self, name: &str) -> Option<StoredCredential> {
        let entry = self.0.accounts.get(name)?;
        match (&entry.api_key, &entry.oauth) {
            (Some(key), _) if !key.is_empty() => Some(StoredCredential::ApiKey(key.clone())),
            (_, Some(oauth)) => Some(StoredCredential::OAuth(OAuthTokens {
                access: oauth.access.clone(),
                refresh: oauth.refresh.clone(),
                expires_at: oauth.expires_at,
                extra: oauth.extra.clone(),
            })),
            _ => None,
        }
    }

    /// Kept apart from accounts, so an account named `telemetry` cannot shadow it.
    pub fn telemetry(&self) -> Option<&str> {
        self.0
            .telemetry
            .as_ref()
            .and_then(|e| e.api_key.as_deref())
            .filter(|k| !k.is_empty())
    }
}

/// An entry holds exactly one of `api_key` and `oauth`, and `[telemetry]` only an `api_key`.
fn check(file: &File) -> bool {
    let one = |e: &Entry| e.api_key.is_some() != e.oauth.is_some();
    file.accounts.values().all(one)
        && file
            .telemetry
            .as_ref()
            .is_none_or(|e| one(e) && e.oauth.is_none())
}

fn parse_toml(text: &str) -> Result<File, Option<(usize, usize)>> {
    let file: File = toml::from_str(text).map_err(|e| Some(line_col(text, &e)))?;
    if check(&file) { Ok(file) } else { Err(None) }
}

fn parse(text: &str, path: PathBuf) -> Result<File, ConfigError> {
    parse_toml(text).map_err(|position| ConfigError::Credentials {
        path,
        position,
        message: MALFORMED.to_string(),
    })
}

/// Holds the exclusive lock on `<data_dir>/credentials.lock` until dropped. Every writer of
/// `credentials.toml` takes it, so a rotated refresh token is never overwritten by a stale
/// read-modify-write.
pub struct Lock(#[allow(dead_code)] crate::file_lock::FileLock);

/// Blocks until the lock is held.
pub fn lock(data_dir: &Path) -> Result<Lock, ConfigError> {
    let path = data_dir.join(CREDENTIALS_FILE);
    crate::file_lock::lock(&path)
        .map(Lock)
        .map_err(|source| ConfigError::Write {
            path: path.with_extension("lock"),
            source,
        })
}

/// Stores `secret` as the API key of account `name`, keeping every other entry. Read-modify-write
/// through the owner-only writer. A trailing newline is trimmed; an empty secret is a usage error
/// and an unparseable existing file is a config error, neither touching the file.
pub fn store(data_dir: &Path, name: &str, secret: &str) -> Result<(), ConfigError> {
    let secret = secret.trim_end_matches(['\n', '\r']);
    if secret.is_empty() {
        return Err(ConfigError::usage(
            "the secret is empty; nothing was stored",
        ));
    }
    let entry = Entry {
        api_key: Some(secret.to_string()),
        oauth: None,
    };
    let _lock = lock(data_dir)?;
    write_entry(data_dir, name, entry)
}

/// Stores a sign-in's tokens for `name`, replacing whatever the account had.
pub fn store_oauth(data_dir: &Path, name: &str, tokens: &OAuthTokens) -> Result<(), ConfigError> {
    let lock = lock(data_dir)?;
    store_oauth_locked(data_dir, name, tokens, &lock)
}

/// [`store_oauth`] for a caller that already holds the lock, which it proves by passing it.
pub fn store_oauth_locked(
    data_dir: &Path,
    name: &str,
    tokens: &OAuthTokens,
    _lock: &Lock,
) -> Result<(), ConfigError> {
    let entry = Entry {
        api_key: None,
        oauth: Some(tokens.into()),
    };
    write_entry(data_dir, name, entry)
}

fn write_entry(data_dir: &Path, name: &str, entry: Entry) -> Result<(), ConfigError> {
    let path = credentials_path(data_dir);
    let read_err = |source| ConfigError::Read {
        path: path.clone(),
        source,
    };
    let mut file = match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text, path.clone())?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => File::default(),
        Err(e) => return Err(read_err(e)),
    };
    file.accounts.insert(name.to_string(), entry);
    write_file(data_dir, &path, &file)
}

fn write_file(data_dir: &Path, path: &Path, file: &File) -> Result<(), ConfigError> {
    let write_err = |source| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    };
    let text = toml::to_string(file).map_err(|e| ConfigError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    std::fs::create_dir_all(data_dir).map_err(write_err)?;
    crate::owner_only::write_atomic(path, text.as_bytes()).map_err(write_err)
}

/// Removes account `name`'s entry and keeps every other one. `false` when nothing was stored.
/// An unparseable existing file is a config error and is left untouched.
pub fn remove(data_dir: &Path, name: &str) -> Result<bool, ConfigError> {
    let _lock = lock(data_dir)?;
    let path = credentials_path(data_dir);
    let mut file = match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text, path.clone())?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(ConfigError::Read { path, source }),
    };
    if file.accounts.remove(name).is_none() {
        return Ok(false);
    }
    write_file(data_dir, &path, &file)?;
    Ok(true)
}

/// Schema check for `credentials.toml` text, used by the docs check.
pub fn validate_credentials_toml(text: &str) -> Result<(), String> {
    parse_toml(text)
        .map(|_| ())
        .map_err(|position| match position {
            Some((line, column)) => format!("{line}:{column}: {MALFORMED}"),
            None => MALFORMED.to_string(),
        })
}

/// 1-based position of the error. `toml::de::Error`'s `Display` quotes the source line, which
/// would leak the key, so only the position is ever reported.
fn line_col(text: &str, e: &toml::de::Error) -> (usize, usize) {
    let start = e.span().map_or(0, |s| s.start).min(text.len());
    let before = &text.as_bytes()[..start];
    let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
    let column = start
        - before
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1)
        + 1;
    (line, column)
}
