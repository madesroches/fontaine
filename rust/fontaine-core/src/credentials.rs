use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::ConfigError;

pub const CREDENTIALS_FILE: &str = "credentials.toml";

pub fn credentials_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CREDENTIALS_FILE)
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    accounts: BTreeMap<String, Entry>,
    telemetry: Option<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    api_key: String,
}

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
            Err(source) => return Err(ConfigError::Io { path, source }),
        };
        toml::from_str(&text)
            .map(Credentials)
            .map_err(|e| ConfigError::Parse {
                path,
                message: e.to_string(),
            })
    }

    pub fn account(&self, name: &str) -> Option<&str> {
        non_empty(self.0.accounts.get(name))
    }

    /// Kept apart from accounts, so an account named `telemetry` cannot shadow it.
    pub fn telemetry(&self) -> Option<&str> {
        non_empty(self.0.telemetry.as_ref())
    }
}

fn non_empty(entry: Option<&Entry>) -> Option<&str> {
    entry.map(|e| e.api_key.as_str()).filter(|k| !k.is_empty())
}

/// Schema check for `credentials.toml` text, used by the docs check.
pub fn validate_credentials_toml(text: &str) -> Result<(), String> {
    toml::from_str::<File>(text)
        .map(|_| ())
        .map_err(|e| e.to_string())
}
