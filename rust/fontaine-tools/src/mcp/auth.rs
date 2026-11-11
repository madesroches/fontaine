//! OAuth HTTP through the user's `[network]` settings. Every metadata, authorize, registration
//! and token request rmcp's authorization manager makes goes through one client built here, so
//! the proxy, extra CA roots and client certificate of [`LoadedNetwork`] apply. It is a build
//! separate from the protocol client, so declared MCP headers never reach the authorization
//! server.
//!
//! Because a custom client loses the address filtering of rmcp's default client, this adapter
//! replicates the metadata-address policy at the host-string level: a discovered
//! metadata/token/authorize endpoint at a private, link-local or cloud-metadata address is
//! rejected unless the base MCP server URL is itself loopback — which is also what keeps local
//! test servers on `127.0.0.1` working.

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use futures::StreamExt;
use http::Response;
use rmcp::transport::auth::{
    AuthError, CredentialRefreshGuard, CredentialStore, OAuthHttpClient, OAuthHttpClientError,
    OAuthHttpClientFuture, OAuthHttpRedirectPolicy, OAuthHttpRequest, StateStore,
    StoredAuthorizationState, StoredCredentials,
};
use url::{Host, Url};

use crate::tls::{LoadedNetwork, build_client};

const MAX_OAUTH_HTTP_RESPONSE_BODY_BYTES: usize = 1024 * 1024;
const MAX_OAUTH_REDIRECTS: usize = 10;
/// Matches the SDK default; OAuth requests are rare, so a fixed cap is fine.
const OAUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

const CLOUD_METADATA_HOSTS: &[&str] = &[
    "metadata",
    "metadata.google.internal",
    "metadata.azure.internal",
];

/// Executes the authorization manager's outbound HTTP with the network policy applied and the
/// metadata-address guard enforced.
pub struct OAuthClient {
    follow_redirects: reqwest::Client,
    stop_redirects: reqwest::Client,
    base_url: Url,
}

/// Why the OAuth HTTP client could not be built.
#[derive(Debug)]
pub enum OAuthClientError {
    BadUrl {
        url: String,
        source: url::ParseError,
    },
    Build(reqwest::Error),
}

impl fmt::Display for OAuthClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadUrl { url, .. } => write!(f, "`{url}` is not a URL"),
            Self::Build(_) => f.write_str("could not build the OAuth HTTP client"),
        }
    }
}

impl std::error::Error for OAuthClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BadUrl { source, .. } => Some(source),
            Self::Build(source) => Some(source),
        }
    }
}

/// Builds the adapter for a server at `base_url`. `network` comes from the loaded `[network]`
/// settings.
pub fn build_oauth_client(
    network: &LoadedNetwork,
    base_url: &str,
) -> Result<OAuthClient, OAuthClientError> {
    let base_url = Url::parse(base_url).map_err(|source| OAuthClientError::BadUrl {
        url: base_url.to_string(),
        source,
    })?;
    let policy = Arc::new(AddressPolicy::new(&base_url));
    let follow_policy = policy.clone();
    let follow_redirects = build_client(network, move || {
        let policy = follow_policy.clone();
        let redirect = reqwest::redirect::Policy::custom(move |attempt| {
            if !policy.allows(attempt.url()) {
                let url = attempt.url().clone();
                attempt.error(DisallowedAddress(url))
            } else if attempt.previous().len() >= MAX_OAUTH_REDIRECTS {
                attempt.error(TooManyRedirects)
            } else {
                attempt.follow()
            }
        });
        reqwest::Client::builder()
            .timeout(OAUTH_TIMEOUT)
            .redirect(redirect)
    })
    .map_err(OAuthClientError::Build)?;
    let stop_redirects = build_client(network, || {
        reqwest::Client::builder()
            .timeout(OAUTH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
    })
    .map_err(OAuthClientError::Build)?;
    Ok(OAuthClient {
        follow_redirects,
        stop_redirects,
        base_url,
    })
}

#[derive(Debug)]
struct DisallowedAddress(Url);

impl fmt::Display for DisallowedAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "OAuth request to `{}` rejected: address not allowed for this resource",
            self.0
        )
    }
}

impl std::error::Error for DisallowedAddress {}

#[derive(Debug)]
struct TooManyRedirects;

impl fmt::Display for TooManyRedirects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OAuth request exceeded {MAX_OAUTH_REDIRECTS} redirects")
    }
}

impl std::error::Error for TooManyRedirects {}

#[derive(Debug)]
struct ResponseBodyTooLarge;

impl fmt::Display for ResponseBodyTooLarge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "OAuth HTTP response body exceeds {MAX_OAUTH_HTTP_RESPONSE_BODY_BYTES} bytes"
        )
    }
}

impl std::error::Error for ResponseBodyTooLarge {}

/// The replicated metadata-address policy: what may be reached, given the class of the base
/// MCP server URL's host.
struct AddressPolicy {
    base_origin: url::Origin,
    base_is_loopback: bool,
}

impl AddressPolicy {
    fn new(base_url: &Url) -> AddressPolicy {
        AddressPolicy {
            base_origin: base_url.origin(),
            base_is_loopback: base_url.host().is_some_and(is_loopback_host),
        }
    }

    fn allows(&self, url: &Url) -> bool {
        let Some(host) = url.host() else {
            return false;
        };
        // The server's own origin is always reachable, as in the SDK: a private-IP server could
        // never complete OAuth otherwise.
        if url.origin() == self.base_origin {
            return true;
        }
        !is_disallowed_host(host.clone()) || (self.base_is_loopback && is_loopback_host(host))
    }
}

fn is_loopback_host(host: Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost" || domain.ends_with(".localhost")
        }
        Host::Ipv4(addr) => addr.is_loopback(),
        Host::Ipv6(addr) => addr.is_loopback(),
    }
}

/// Replicates the SDK's `is_disallowed_metadata_host`: private, loopback, link-local, reserved,
/// broadcast, multicast and cloud-metadata hosts.
fn is_disallowed_host(host: Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost"
                || domain.ends_with(".localhost")
                || CLOUD_METADATA_HOSTS.contains(&domain.as_str())
        }
        Host::Ipv4(addr) => is_disallowed_ipv4(addr),
        Host::Ipv6(addr) => is_disallowed_ipv6(addr),
    }
}

fn is_disallowed_ipv4(addr: Ipv4Addr) -> bool {
    let octets = addr.octets();
    addr.is_private()
        || addr.is_loopback()
        || addr.is_link_local()
        || addr.is_broadcast()
        || addr.is_unspecified()
        || addr.is_multicast()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 198 && matches!(octets[1], 18 | 19))
}

/// The IPv4 address embedded by IPv4-mapped, IPv4-compatible, NAT64 (`64:ff9b::/96`) and 6to4
/// (`2002::/16`) addresses.
fn embedded_ipv4(addr: Ipv6Addr) -> Option<Ipv4Addr> {
    let [a, b, c, d, e, f, g, h] = addr.segments();
    let low = Ipv4Addr::from((u32::from(g) << 16) | u32::from(h));
    match [a, b, c, d, e, f] {
        [0, 0, 0, 0, 0, 0xffff] | [0x64, 0xff9b, 0, 0, 0, 0] => Some(low),
        [0, 0, 0, 0, 0, 0] if !addr.is_loopback() && !addr.is_unspecified() => Some(low),
        [0x2002, ..] => Some(Ipv4Addr::from((u32::from(b) << 16) | u32::from(c))),
        _ => None,
    }
}

fn is_disallowed_ipv6(addr: Ipv6Addr) -> bool {
    if let Some(embedded) = embedded_ipv4(addr) {
        return is_disallowed_ipv4(embedded);
    }
    let segments = addr.segments();
    addr.is_loopback()
        || addr.is_unspecified()
        || addr.is_multicast()
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xfe00) == 0xfc00
        || segments[..2] == [0x2001, 0]
        || segments[..3] == [0x64, 0xff9b, 1]
}

impl OAuthHttpClient for OAuthClient {
    fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        Box::pin(async move {
            let OAuthHttpRequest {
                request,
                redirect_policy,
                ..
            } = request;
            let request = reqwest::Request::try_from(request)
                .map_err(|e| Box::new(e) as OAuthHttpClientError)?;
            if !AddressPolicy::new(&self.base_url).allows(request.url()) {
                return Err(
                    Box::new(DisallowedAddress(request.url().clone())) as OAuthHttpClientError
                );
            }
            let client = match redirect_policy {
                OAuthHttpRedirectPolicy::Stop => &self.stop_redirects,
                // The SDK default is Follow; an unknown future policy follows too.
                _ => &self.follow_redirects,
            };
            let response = client
                .execute(request)
                .await
                .map_err(|e| Box::new(e) as OAuthHttpClientError)?;
            let mut builder = Response::builder()
                .status(response.status())
                .version(response.version());
            for (name, value) in response.headers() {
                builder = builder.header(name, value);
            }
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| Box::new(e) as OAuthHttpClientError)?;
                if chunk.len() > MAX_OAUTH_HTTP_RESPONSE_BODY_BYTES - body.len() {
                    return Err(Box::new(ResponseBodyTooLarge) as OAuthHttpClientError);
                }
                body.extend_from_slice(&chunk);
            }
            builder
                .body(body)
                .map_err(|e| Box::new(e) as OAuthHttpClientError)
        })
    }
}

/// Shared `CredentialStore`/`StateStore` handles: rmcp's setters take a value, not an `Arc`, so
/// the stores cross the crate boundary as these transparent delegating wrappers.
#[derive(Clone)]
pub struct SharedCredentialStore(pub Arc<dyn CredentialStore>);

impl fmt::Debug for SharedCredentialStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SharedCredentialStore(..)")
    }
}

#[async_trait::async_trait]
impl CredentialStore for SharedCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        self.0.load().await
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        self.0.save(credentials).await
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.0.clear().await
    }

    async fn acquire_refresh_guard(&self) -> Result<Option<CredentialRefreshGuard>, AuthError> {
        self.0.acquire_refresh_guard().await
    }
}

#[derive(Clone)]
pub struct SharedStateStore(pub Arc<dyn StateStore>);

impl fmt::Debug for SharedStateStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SharedStateStore(..)")
    }
}

#[async_trait::async_trait]
impl StateStore for SharedStateStore {
    async fn save(
        &self,
        csrf_token: &str,
        state: StoredAuthorizationState,
    ) -> Result<(), AuthError> {
        self.0.save(csrf_token, state).await
    }

    async fn load(&self, csrf_token: &str) -> Result<Option<StoredAuthorizationState>, AuthError> {
        self.0.load(csrf_token).await
    }

    async fn delete(&self, csrf_token: &str) -> Result<(), AuthError> {
        self.0.delete(csrf_token).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn allows(base: &str, target: &str) -> bool {
        AddressPolicy::new(&Url::parse(base).unwrap()).allows(&Url::parse(target).unwrap())
    }

    #[test]
    fn public_endpoints_are_allowed_for_any_base() {
        for base in ["https://mcp.example.com/mcp", "http://127.0.0.1:3111/mcp"] {
            assert!(allows(base, "https://auth.example.com/token"), "{base}");
            assert!(
                allows(base, "https://auth.example.com:8443/token"),
                "{base}"
            );
        }
    }

    #[test]
    fn a_private_base_reaches_its_own_origin_only() {
        let base = "https://10.0.0.5/mcp";
        assert!(allows(
            base,
            "https://10.0.0.5/.well-known/oauth-protected-resource"
        ));
        assert!(!allows(base, "https://10.0.0.6/token"));
        assert!(!allows(base, "http://10.0.0.5/token"));
        assert!(!allows(base, "https://10.0.0.5:8443/token"));
    }

    #[test]
    fn private_and_link_local_endpoints_are_rejected_for_a_remote_base() {
        for target in [
            "http://169.254.169.254/latest",
            "http://100.100.100.200/",
            "http://192.168.1.1/token",
            "http://10.0.0.9/token",
            "http://[fe80::1]/token",
            "http://[fd00::1]/token",
            "http://[::1]/token",
            "http://127.0.0.1/token",
            "http://localhost/token",
            "http://foo.localhost/token",
            "http://metadata.google.internal/",
            "http://0.0.0.0/",
            "http://239.1.1.1/",
            "http://198.18.0.1/",
            "http://[2001::1]/",
        ] {
            assert!(!allows("https://mcp.example.com/mcp", target), "{target}");
        }
    }

    #[test]
    fn loopback_endpoints_are_allowed_when_the_base_is_loopback() {
        // The wiremock case: a loopback authorization server for a loopback MCP server.
        for target in [
            "http://127.0.0.1:3111/token",
            "http://[::1]:3111/token",
            "http://localhost:3111/token",
        ] {
            assert!(allows("http://127.0.0.1:9/mcp", target), "{target}");
        }
        // Private (non-loopback) addresses stay refused even for a loopback base.
        for target in ["http://192.168.1.1/token", "http://[fe80::1]/token"] {
            assert!(!allows("http://127.0.0.1:9/mcp", target), "{target}");
        }
    }

    #[test]
    fn the_disallowed_address_sets_match_the_sdk() {
        for addr in [
            Ipv4Addr::new(10, 1, 2, 3),
            Ipv4Addr::new(172, 16, 0, 1),
            Ipv4Addr::new(192, 168, 0, 1),
            Ipv4Addr::new(127, 0, 0, 1),
            Ipv4Addr::new(169, 254, 1, 1),
            Ipv4Addr::new(100, 64, 0, 1),
            Ipv4Addr::new(198, 18, 0, 1),
            Ipv4Addr::new(224, 0, 0, 1),
            Ipv4Addr::new(0, 0, 0, 0),
            Ipv4Addr::new(255, 255, 255, 255),
        ] {
            assert!(is_disallowed_ipv4(addr), "{addr}");
        }
        for addr in [Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(100, 63, 0, 1)] {
            assert!(!is_disallowed_ipv4(addr), "{addr}");
        }
        for addr in [
            Ipv6Addr::LOCALHOST,
            Ipv6Addr::UNSPECIFIED,
            "fe80::1".parse().unwrap(),
            "fd00::1".parse().unwrap(),
            "ff02::1".parse().unwrap(),
            "2001::1".parse().unwrap(),
            "::ffff:10.0.0.1".parse().unwrap(),
            "2002:0a00:0001::1".parse().unwrap(),
        ] {
            assert!(is_disallowed_ipv6(addr), "{addr}");
        }
        assert!(!is_disallowed_ipv6("2606:4700::1".parse().unwrap()));
        assert_eq!(
            embedded_ipv4("64:ff9b::0808:0808".parse().unwrap()),
            Some(Ipv4Addr::new(8, 8, 8, 8))
        );
    }
}
