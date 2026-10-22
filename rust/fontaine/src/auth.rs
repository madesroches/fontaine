use std::io::{IsTerminal, Read};
use std::path::Path;

use fontaine_core::config::{Config, ConfigError};
use fontaine_core::credentials::{self, Target};

/// `fontaine auth login`: stores a secret read from a hidden prompt, or from stdin when stdin
/// is not a terminal.
pub fn login(
    account: Option<&str>,
    telemetry: bool,
    config: &Config,
    data_dir: &Path,
) -> Result<(), ConfigError> {
    let target = match (account, telemetry) {
        (_, true) => Target::Telemetry,
        (Some(name), false) => {
            if !config.accounts.contains_key(name) {
                let known: Vec<&str> = config.accounts.keys().map(String::as_str).collect();
                return Err(ConfigError::usage(format!(
                    "unknown account `{name}`; configured accounts: {}",
                    if known.is_empty() {
                        "none".to_string()
                    } else {
                        known.join(", ")
                    }
                )));
            }
            Target::Account(name)
        }
        (None, false) => {
            return Err(ConfigError::usage("name an account or pass --telemetry"));
        }
    };

    let secret = read_secret(&target)?;
    credentials::store(data_dir, target, &secret)?;
    eprintln!(
        "stored the {} in {}",
        match target {
            Target::Account(name) => format!("credential for account `{name}`"),
            Target::Telemetry => "telemetry ingestion key".to_string(),
        },
        credentials::credentials_path(data_dir).display()
    );
    if let Target::Account(name) = target
        && let Some(var) = config
            .accounts
            .get(name)
            .and_then(|a| a.api_key_env.as_deref())
    {
        eprintln!(
            "fontaine: warning: account `{name}` sets `api_key_env = \"{var}\"`, so the stored key is ignored while it is set"
        );
    }
    Ok(())
}

fn read_secret(target: &Target<'_>) -> Result<String, ConfigError> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        let prompt = match target {
            Target::Account(name) => format!("API key for `{name}`: "),
            Target::Telemetry => "Telemetry ingestion key: ".to_string(),
        };
        return rpassword::prompt_password(prompt)
            .map_err(|e| ConfigError::usage(format!("cannot read the secret: {e}")));
    }
    let mut text = String::new();
    stdin
        .lock()
        .read_to_string(&mut text)
        .map_err(|e| ConfigError::usage(format!("cannot read the secret from stdin: {e}")))?;
    Ok(text)
}
