//! Client construction that survives a host with no system CA certificates.

use crate::error_chain::chain;

/// Builds the client from `make()`; if that fails (on Unix, reqwest loads the system roots at
/// build time), retries trusting the bundled Mozilla roots and warns. A closure because
/// `ClientBuilder` is not `Clone`. If the retry also fails, the first error is returned.
pub fn build_client(make: impl Fn() -> reqwest::ClientBuilder) -> reqwest::Result<reqwest::Client> {
    let error = match make().build() {
        Ok(client) => return Ok(client),
        Err(error) => error,
    };
    let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .filter_map(|der| reqwest::Certificate::from_der(der).ok());
    let Ok(client) = make().tls_certs_only(roots).build() else {
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

    #[test]
    fn retries_with_bundled_roots_when_the_first_build_fails() {
        let calls = AtomicUsize::new(0);
        let client = build_client(|| {
            let builder = reqwest::Client::builder();
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                builder.user_agent("bad\nvalue")
            } else {
                builder
            }
        });
        assert!(client.is_ok(), "{:?}", client.err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn builds_once_when_the_first_build_works() {
        let calls = AtomicUsize::new(0);
        build_client(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            reqwest::Client::builder()
        })
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
