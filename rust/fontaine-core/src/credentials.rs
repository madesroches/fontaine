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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    api_key: String,
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
            Err(source) => return Err(ConfigError::Io { path, source }),
        };
        parse(&text, path).map(Credentials)
    }

    pub fn account(&self, name: &str) -> Option<&str> {
        non_empty(self.0.accounts.get(name))
    }

    /// Kept apart from accounts, so an account named `telemetry` cannot shadow it.
    pub fn telemetry(&self) -> Option<&str> {
        non_empty(self.0.telemetry.as_ref())
    }
}

fn parse(text: &str, path: PathBuf) -> Result<File, ConfigError> {
    toml::from_str(text).map_err(|e| {
        let (line, column) = line_col(text, &e);
        ConfigError::Credentials {
            path,
            line,
            column,
            message: MALFORMED.to_string(),
        }
    })
}

/// Which credential a stored secret belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target<'a> {
    Account(&'a str),
    Telemetry,
}

/// Stores `secret` for `target`, keeping every other entry. Read-modify-write through the
/// owner-only writer. A trailing newline is trimmed; an empty secret is a usage error and an
/// unparseable existing file is a config error, neither touching the file.
pub fn store(data_dir: &Path, target: Target<'_>, secret: &str) -> Result<(), ConfigError> {
    let secret = secret.trim_end_matches(['\n', '\r']);
    if secret.is_empty() {
        return Err(ConfigError::usage(
            "the secret is empty; nothing was stored",
        ));
    }
    let path = credentials_path(data_dir);
    let io_err = |source| ConfigError::Io {
        path: path.clone(),
        source,
    };
    let mut file = match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text, path.clone())?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => File::default(),
        Err(e) => return Err(io_err(e)),
    };
    let entry = Entry {
        api_key: secret.to_string(),
    };
    match target {
        Target::Account(name) => {
            file.accounts.insert(name.to_string(), entry);
        }
        Target::Telemetry => file.telemetry = Some(entry),
    }
    let text = toml::to_string(&file).map_err(|e| ConfigError::Parse {
        path: path.clone(),
        message: e.to_string(),
    })?;
    std::fs::create_dir_all(data_dir).map_err(io_err)?;
    crate::owner_only::write(&path, text.as_bytes()).map_err(io_err)
}

fn non_empty(entry: Option<&Entry>) -> Option<&str> {
    entry.map(|e| e.api_key.as_str()).filter(|k| !k.is_empty())
}

/// Schema check for `credentials.toml` text, used by the docs check.
pub fn validate_credentials_toml(text: &str) -> Result<(), String> {
    toml::from_str::<File>(text).map(|_| ()).map_err(|e| {
        let (line, column) = line_col(text, &e);
        format!("{line}:{column}: {MALFORMED}")
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
