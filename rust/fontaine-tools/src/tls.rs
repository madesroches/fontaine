//! Client construction that survives a host with no system CA certificates, and applies the
//! user's `[network]` settings (proxy, extra CA roots, client certificate).

use crate::error_chain::chain;
use std::fmt;
use std::path::{Path, PathBuf};

/// The network keys as written in config, before any file is read. `Default` is "nothing set":
/// reqwest's own environment and system proxy handling stays in force.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetworkSettings {
    pub https_proxy: Option<String>,
    pub http_proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub ca_bundle: Option<PathBuf>,
    pub client_cert: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
}

impl NetworkSettings {
    /// Each key set in `over` replaces this one's; the rest are kept.
    pub fn overlaid_by(&self, over: &NetworkSettings) -> NetworkSettings {
        NetworkSettings {
            https_proxy: over
                .https_proxy
                .clone()
                .or_else(|| self.https_proxy.clone()),
            http_proxy: over.http_proxy.clone().or_else(|| self.http_proxy.clone()),
            no_proxy: over.no_proxy.clone().or_else(|| self.no_proxy.clone()),
            ca_bundle: over.ca_bundle.clone().or_else(|| self.ca_bundle.clone()),
            client_cert: over
                .client_cert
                .clone()
                .or_else(|| self.client_cert.clone()),
            client_key: over.client_key.clone().or_else(|| self.client_key.clone()),
        }
    }

    /// Reads the files, parses the PEM and the proxy URLs, and applies the cross-key rules.
    pub fn load(&self) -> Result<LoadedNetwork, NetworkError> {
        let has_proxy = self.https_proxy.is_some() || self.http_proxy.is_some();
        if self.no_proxy.is_some() && !has_proxy {
            return Err(NetworkError::NoProxyWithoutProxy);
        }
        let mut proxies = Vec::new();
        for (key, url, https) in [
            ("https_proxy", &self.https_proxy, true),
            ("http_proxy", &self.http_proxy, false),
        ] {
            let Some(url) = url else { continue };
            let proxy = if https {
                reqwest::Proxy::https(url)
            } else {
                reqwest::Proxy::http(url)
            }
            .map_err(|source| NetworkError::Proxy {
                key,
                source: source.without_url(),
            })?;
            let no_proxy = self
                .no_proxy
                .as_deref()
                .and_then(reqwest::NoProxy::from_string);
            proxies.push(proxy.no_proxy(no_proxy));
        }

        let roots = match &self.ca_bundle {
            None => Vec::new(),
            Some(path) => {
                let pem = read("ca_bundle", path)?;
                let roots = reqwest::Certificate::from_pem_bundle(&pem).map_err(|source| {
                    NetworkError::Pem {
                        key: "ca_bundle",
                        path: path.clone(),
                        source: source.without_url(),
                    }
                })?;
                if roots.is_empty() {
                    return Err(NetworkError::NoCertificates { path: path.clone() });
                }
                roots
            }
        };

        let identity = match (&self.client_cert, &self.client_key) {
            (None, None) => None,
            (Some(cert_path), Some(key_path)) => {
                let mut pem = read("client_cert", cert_path)?;
                let key = read("client_key", key_path)?;
                if is_encrypted(&key) {
                    return Err(NetworkError::EncryptedKey {
                        path: key_path.clone(),
                    });
                }
                pem.push(b'\n');
                pem.extend_from_slice(&key);
                let identity =
                    reqwest::Identity::from_pem(&pem).map_err(|source| NetworkError::Pem {
                        key: "client_key",
                        path: key_path.clone(),
                        source: source.without_url(),
                    })?;
                Some(identity)
            }
            _ => return Err(NetworkError::HalfIdentity),
        };
        Ok(LoadedNetwork {
            proxies,
            roots,
            identity,
        })
    }
}

fn read(key: &'static str, path: &Path) -> Result<Vec<u8>, NetworkError> {
    std::fs::read(path).map_err(|source| NetworkError::Read {
        key,
        path: path.to_owned(),
        source,
    })
}

fn is_encrypted(key_pem: &[u8]) -> bool {
    let text = String::from_utf8_lossy(key_pem);
    text.contains("ENCRYPTED PRIVATE KEY") || text.contains("Proc-Type: 4,ENCRYPTED")
}

/// Settings that passed [`NetworkSettings::load`]: parsed proxies, CA roots and identity.
#[derive(Clone, Debug, Default)]
pub struct LoadedNetwork {
    proxies: Vec<reqwest::Proxy>,
    roots: Vec<reqwest::Certificate>,
    identity: Option<reqwest::Identity>,
}

impl LoadedNetwork {
    pub fn apply(&self, mut builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        for proxy in &self.proxies {
            builder = builder.proxy(proxy.clone());
        }
        if !self.roots.is_empty() {
            builder = builder.tls_certs_merge(self.roots.iter().cloned());
        }
        match &self.identity {
            Some(identity) => builder.identity(identity.clone()),
            None => builder,
        }
    }
}

/// Typed because callers surface it at config load with a distinct exit code.
#[derive(Debug)]
pub enum NetworkError {
    Read {
        key: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    NoCertificates {
        path: PathBuf,
    },
    Pem {
        key: &'static str,
        path: PathBuf,
        source: reqwest::Error,
    },
    EncryptedKey {
        path: PathBuf,
    },
    Proxy {
        key: &'static str,
        source: reqwest::Error,
    },
    HalfIdentity,
    NoProxyWithoutProxy,
}

impl fmt::Display for NetworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetworkError::Read { key, path, .. } => {
                write!(f, "{key}: cannot read {}", path.display())
            }
            NetworkError::NoCertificates { path } => {
                write!(f, "ca_bundle: {} contains no certificates", path.display())
            }
            NetworkError::Pem { key, path, .. } => {
                write!(f, "{key}: {} is not valid PEM", path.display())
            }
            NetworkError::EncryptedKey { path } => write!(
                f,
                "client_key: {} is passphrase-protected, which is not supported; \
                 use an unencrypted PEM key",
                path.display()
            ),
            NetworkError::Proxy { key, .. } => write!(f, "{key}: not a valid proxy URL"),
            NetworkError::HalfIdentity => {
                f.write_str("client_cert and client_key must be set together")
            }
            NetworkError::NoProxyWithoutProxy => {
                f.write_str("no_proxy is set but neither https_proxy nor http_proxy is")
            }
        }
    }
}

impl std::error::Error for NetworkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            NetworkError::Read { source, .. } => Some(source),
            NetworkError::Pem { source, .. } | NetworkError::Proxy { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Builds the client from `make()` with `network` applied; if that fails (on Unix, reqwest
/// loads the system roots at build time), retries trusting the bundled Mozilla roots plus the
/// custom ones and warns. A closure because `ClientBuilder` is not `Clone`. If the retry also
/// fails, the first error is returned.
pub fn build_client(
    network: &LoadedNetwork,
    make: impl Fn() -> reqwest::ClientBuilder,
) -> reqwest::Result<reqwest::Client> {
    build_with(|builder| network.apply(builder), make)
}

fn build_with(
    apply: impl Fn(reqwest::ClientBuilder) -> reqwest::ClientBuilder,
    make: impl Fn() -> reqwest::ClientBuilder,
) -> reqwest::Result<reqwest::Client> {
    let error = match apply(make()).build() {
        Ok(client) => return Ok(client),
        Err(error) => error,
    };
    let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .filter_map(|der| reqwest::Certificate::from_der(der).ok());
    let Ok(client) = apply(make().tls_certs_only(roots)).build() else {
        return Err(error);
    };
    tracing::warn!(
        "the system's CA certificates could not be loaded ({}), so the bundled Mozilla roots \
         are trusted; install the system's CA certificates, or set SSL_CERT_FILE or \
         SSL_CERT_DIR, to trust those instead",
        chain(&error)
    );
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn bad_first_build(calls: &AtomicUsize) -> reqwest::ClientBuilder {
        let builder = reqwest::Client::builder();
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            builder.user_agent("bad\nvalue")
        } else {
            builder
        }
    }

    #[test]
    fn retries_with_bundled_roots_when_the_first_build_fails() {
        let calls = AtomicUsize::new(0);
        let client = build_client(&LoadedNetwork::default(), || bad_first_build(&calls));
        assert!(client.is_ok(), "{:?}", client.err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_retry_applies_the_network_settings_again() {
        let calls = AtomicUsize::new(0);
        let applied = AtomicUsize::new(0);
        build_with(
            |builder| {
                applied.fetch_add(1, Ordering::SeqCst);
                builder
            },
            || bad_first_build(&calls),
        )
        .unwrap();
        assert_eq!(applied.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn builds_once_when_the_first_build_works() {
        let calls = AtomicUsize::new(0);
        build_client(&LoadedNetwork::default(), || {
            calls.fetch_add(1, Ordering::SeqCst);
            reqwest::Client::builder()
        })
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    fn settings() -> NetworkSettings {
        NetworkSettings::default()
    }

    fn file(dir: &tempfile::TempDir, name: &str, text: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn an_overlay_replaces_only_the_keys_it_sets() {
        let base = NetworkSettings {
            https_proxy: Some("http://a:1".into()),
            no_proxy: Some("x".into()),
            ca_bundle: Some("ca.pem".into()),
            ..settings()
        };
        let over = NetworkSettings {
            https_proxy: Some("http://b:2".into()),
            client_cert: Some("c.pem".into()),
            ..settings()
        };
        let merged = base.overlaid_by(&over);
        assert_eq!(merged.https_proxy.as_deref(), Some("http://b:2"));
        assert_eq!(merged.no_proxy.as_deref(), Some("x"));
        assert_eq!(merged.ca_bundle, Some("ca.pem".into()));
        assert_eq!(merged.client_cert, Some("c.pem".into()));
        assert_eq!(merged.http_proxy, None);
    }

    #[test]
    fn nothing_set_loads_to_nothing() {
        let loaded = settings().load().unwrap();
        assert!(loaded.proxies.is_empty() && loaded.roots.is_empty());
        assert!(loaded.identity.is_none());
    }

    #[test]
    fn no_proxy_needs_a_proxy_url() {
        let s = NetworkSettings {
            no_proxy: Some("localhost".into()),
            ..settings()
        };
        assert!(matches!(s.load(), Err(NetworkError::NoProxyWithoutProxy)));
        let with = NetworkSettings {
            https_proxy: Some("http://p:1".into()),
            ..settings()
        };
        assert!(s.overlaid_by(&with).load().is_ok());
    }

    #[test]
    fn an_unparseable_proxy_url_is_an_error() {
        let s = NetworkSettings {
            http_proxy: Some("not a url".into()),
            ..settings()
        };
        assert!(matches!(
            s.load(),
            Err(NetworkError::Proxy {
                key: "http_proxy",
                ..
            })
        ));
    }

    #[test]
    fn one_url_key_builds_one_proxy_and_both_build_two() {
        let one = NetworkSettings {
            https_proxy: Some("http://p:1".into()),
            ..settings()
        };
        assert_eq!(one.load().unwrap().proxies.len(), 1);
        let both = NetworkSettings {
            http_proxy: Some("http://q:1".into()),
            ..one
        };
        assert_eq!(both.load().unwrap().proxies.len(), 2);
    }

    #[test]
    fn a_missing_ca_file_names_the_key_and_path() {
        let s = NetworkSettings {
            ca_bundle: Some("/nonexistent/ca.pem".into()),
            ..settings()
        };
        let err = s.load().unwrap_err();
        assert!(matches!(
            err,
            NetworkError::Read {
                key: "ca_bundle",
                ..
            }
        ));
        let text = chain(&err);
        assert!(text.contains("ca_bundle") && text.contains("/nonexistent/ca.pem"));
    }

    #[test]
    fn a_ca_file_without_certificates_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        for text in ["", "just some text\n"] {
            let s = NetworkSettings {
                ca_bundle: Some(file(&dir, "ca.pem", text)),
                ..settings()
            };
            assert!(
                matches!(
                    s.load(),
                    Err(NetworkError::NoCertificates { .. } | NetworkError::Pem { .. })
                ),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_cert_without_a_key_and_a_key_without_a_cert_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = file(&dir, "x.pem", "");
        let cert_only = NetworkSettings {
            client_cert: Some(p.clone()),
            ..settings()
        };
        assert!(matches!(cert_only.load(), Err(NetworkError::HalfIdentity)));
        let key_only = NetworkSettings {
            client_key: Some(p),
            ..settings()
        };
        assert!(matches!(key_only.load(), Err(NetworkError::HalfIdentity)));
    }

    #[test]
    fn an_encrypted_key_is_refused_with_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let s = NetworkSettings {
            client_cert: Some(file(&dir, "c.pem", "")),
            client_key: Some(file(
                &dir,
                "k.pem",
                "-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n-----END ENCRYPTED PRIVATE KEY-----\n",
            )),
            ..settings()
        };
        let err = s.load().unwrap_err();
        assert!(matches!(err, NetworkError::EncryptedKey { .. }), "{err}");
    }

    #[test]
    fn a_non_pem_identity_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let s = NetworkSettings {
            client_cert: Some(file(&dir, "c.pem", "junk")),
            client_key: Some(file(&dir, "k.pem", "junk")),
            ..settings()
        };
        assert!(matches!(s.load(), Err(NetworkError::Pem { .. })));
    }
}
