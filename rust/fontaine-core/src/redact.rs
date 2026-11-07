//! Scrubs secrets from text that leaves the machine (a bug report) or lands in a file the user
//! shares (a draft). Known secret values are replaced first, then a token-shape pass catches
//! keys Fontaine was never told about. Nothing here feeds a permission decision.

const REDACTED: &str = "[redacted]";
/// Shorter values are skipped: replacing them would shred ordinary words.
const MIN_SECRET_LEN: usize = 8;
const MIN_TOKEN_TAIL: usize = 16;
const TOKEN_PREFIXES: [&str; 6] = ["sk-", "ghp_", "gho_", "ghs_", "github_pat_", "xox"];

/// A value that must never appear in output.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(value.into())
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// The secrets this session knows: stored credentials, the values of the variables the accounts
/// name with `api_key_env`, MCP server `env` values and the variables they pass through by name,
/// and the scanner token variable. `warn` hears about a credentials store that cannot be read.
pub fn session_secrets(
    config: &crate::config::Config,
    data_dir: &std::path::Path,
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    warn: &dyn Fn(&str),
) -> Vec<Secret> {
    use crate::credentials::{Credentials, StoredCredential};
    let mut out = Vec::new();
    let stored = Credentials::load(data_dir).unwrap_or_else(|e| {
        warn(&format!(
            "stored credentials could not be read, so they are not scrubbed from reports: {}",
            fontaine_tools::error_chain::chain(&e)
        ));
        Credentials::default()
    });
    for (name, account) in &config.accounts {
        match stored.account(name) {
            Some(StoredCredential::ApiKey(key)) => out.push(Secret::new(key)),
            Some(StoredCredential::OAuth(tokens)) => {
                out.push(Secret::new(tokens.access));
                out.push(Secret::new(tokens.refresh));
            }
            None => {}
        }
        if let Some(value) = account.api_key_env.as_deref().and_then(env) {
            out.push(Secret::new(value.to_string_lossy().into_owned()));
        }
    }
    for key in [stored.telemetry(), stored.websearch()]
        .into_iter()
        .flatten()
    {
        out.push(Secret::new(key));
    }
    for server in config.mcp_servers.values() {
        out.extend(
            server
                .env
                .iter()
                .flatten()
                .map(|(_, v)| Secret::new(v.clone())),
        );
        for name in server.env_vars.iter().flatten() {
            if let Some(value) = env(name) {
                out.push(Secret::new(value.to_string_lossy().into_owned()));
            }
        }
    }
    let names = config
        .telemetry
        .api_key_env
        .iter()
        .chain(config.websearch.api_key_env.iter())
        .chain(std::iter::once(&config.guardian.scanner_hf_token_env));
    for name in names {
        if let Some(value) = env(name) {
            out.push(Secret::new(value.to_string_lossy().into_owned()));
        }
    }
    out
}

#[derive(Clone, Debug, Default)]
pub struct Redactor {
    /// Longest first, so a secret that contains another is replaced whole.
    values: Vec<String>,
}

impl Redactor {
    pub fn new(secrets: impl IntoIterator<Item = Secret>) -> Redactor {
        let mut values: Vec<String> = secrets
            .into_iter()
            .map(|s| s.0)
            .filter(|v| v.chars().count() >= MIN_SECRET_LEN)
            .collect();
        values.sort_by_key(|v| std::cmp::Reverse(v.len()));
        values.dedup();
        Redactor { values }
    }

    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for value in &self.values {
            if out.contains(value.as_str()) {
                out = out.replace(value.as_str(), REDACTED);
            }
        }
        scrub_pem(&scrub_tokens(&out))
    }
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '=')
}

/// Replaces `sk-…`/`ghp_…`-shaped tokens and the credential after `Bearer`.
fn scrub_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let at_boundary = out.chars().next_back().is_none_or(|c| !is_token_char(c));
        if at_boundary {
            if let Some(len) = token_len(rest) {
                out.push_str(REDACTED);
                rest = &rest[len..];
                continue;
            }
            if let Some(skip) = bearer_prefix(rest) {
                let tail = rest[skip..]
                    .find(|c: char| !is_token_char(c) && c != '+' && c != '/')
                    .unwrap_or(rest.len() - skip);
                if tail >= MIN_SECRET_LEN {
                    out.push_str(&rest[..skip]);
                    out.push_str(REDACTED);
                    rest = &rest[skip + tail..];
                    continue;
                }
            }
        }
        let c = rest.chars().next().expect("rest is not empty");
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

fn token_len(rest: &str) -> Option<usize> {
    let prefix = TOKEN_PREFIXES.iter().find(|p| rest.starts_with(**p))?;
    let tail = rest[prefix.len()..]
        .find(|c: char| !is_token_char(c))
        .unwrap_or(rest.len() - prefix.len());
    (tail >= MIN_TOKEN_TAIL).then_some(prefix.len() + tail)
}

/// The length of `Bearer ` (any case) when `rest` starts with it.
fn bearer_prefix(rest: &str) -> Option<usize> {
    let head = rest.get(..7)?;
    head.eq_ignore_ascii_case("bearer ").then_some(7)
}

/// Replaces every `-----BEGIN … -----` block through its END line; an unterminated block goes to
/// the end of the text.
fn scrub_pem(text: &str) -> String {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "-----END ";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(BEGIN) {
        out.push_str(&rest[..start]);
        out.push_str(REDACTED);
        let after = &rest[start..];
        rest = match after.find(END) {
            Some(end) => {
                let tail = &after[end + END.len()..];
                match tail.find("-----") {
                    Some(close) => &tail[close + 5..],
                    None => "",
                }
            }
            None => "",
        };
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor(values: &[&str]) -> Redactor {
        Redactor::new(values.iter().map(|v| Secret::new(*v)))
    }

    #[test]
    fn known_values_are_replaced_everywhere() {
        let r = redactor(&["hunter2-long-secret"]);
        let text = "key=hunter2-long-secret and again hunter2-long-secret.";
        assert_eq!(r.redact(text), "key=[redacted] and again [redacted].");
    }

    #[test]
    fn short_values_are_skipped() {
        let r = redactor(&["abc", "1234567"]);
        assert_eq!(r.redact("abc 1234567"), "abc 1234567");
    }

    #[test]
    fn a_longer_secret_wins_over_one_it_contains() {
        let r = redactor(&["secretvalue", "secretvalue-and-more"]);
        assert_eq!(r.redact("x secretvalue-and-more y"), "x [redacted] y");
    }

    #[test]
    fn token_shapes_are_caught() {
        let r = Redactor::default();
        let text = "a sk-abcdefghijklmnopqrstuv b ghp_abcdefghijklmnopqrstuvwx c";
        assert_eq!(r.redact(text), "a [redacted] b [redacted] c");
        // Too short to be a key, and not at a word boundary.
        assert_eq!(r.redact("sk-short"), "sk-short");
        assert_eq!(
            r.redact("task-abcdefghijklmnopqrstuvwxyz"),
            "task-abcdefghijklmnopqrstuvwxyz"
        );
    }

    #[test]
    fn bearer_credentials_are_caught() {
        let r = Redactor::default();
        assert_eq!(
            r.redact("Authorization: Bearer abc123def456ghi789"),
            "Authorization: Bearer [redacted]"
        );
        assert_eq!(r.redact("bearer tiny"), "bearer tiny");
    }

    #[test]
    fn pem_blocks_are_removed() {
        let r = Redactor::default();
        let text = "before\n-----BEGIN PRIVATE KEY-----\nMIIE\n-----END PRIVATE KEY-----\nafter";
        assert_eq!(r.redact(text), "before\n[redacted]\nafter");
        assert_eq!(
            r.redact("x\n-----BEGIN RSA PRIVATE KEY-----\nMIIE"),
            "x\n[redacted]"
        );
    }

    #[test]
    fn multibyte_text_passes_through() {
        let r = Redactor::default();
        assert_eq!(r.redact("héllo — ✓"), "héllo — ✓");
    }

    #[test]
    fn variables_named_by_websearch_and_mcp_are_secrets() {
        use crate::config::{Config, McpServer};
        let mut config = Config::defaults(std::path::PathBuf::from("."));
        config.websearch.api_key_env = Some("SEARCH_KEY".into());
        config.mcp_servers.insert(
            "s".into(),
            McpServer {
                command: Some("x".into()),
                args: None,
                env_vars: Some(vec!["MCP_TOKEN".into()]),
                env: None,
                url: None,
            },
        );
        let env = |name: &str| -> Option<std::ffi::OsString> {
            match name {
                "SEARCH_KEY" => Some("search-key-value".into()),
                "MCP_TOKEN" => Some("mcp-token-value".into()),
                _ => None,
            }
        };
        let data = tempfile::tempdir().unwrap();
        let secrets = session_secrets(&config, data.path(), &env, &|_| {});
        let r = Redactor::new(secrets);
        assert_eq!(
            r.redact("a search-key-value b mcp-token-value c"),
            "a [redacted] b [redacted] c"
        );
    }

    #[test]
    fn an_unreadable_credentials_store_warns() {
        use std::cell::RefCell;
        let config = crate::config::Config::defaults(std::path::PathBuf::from("."));
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("credentials.toml"), "not [valid").unwrap();
        let warned = RefCell::new(Vec::new());
        session_secrets(&config, data.path(), &|_| None, &|m| {
            warned.borrow_mut().push(m.to_string())
        });
        assert_eq!(warned.borrow().len(), 1);
    }

    #[test]
    fn debug_does_not_show_the_value() {
        assert_eq!(format!("{:?}", Secret::new("topsecretvalue")), "Secret(..)");
    }
}
