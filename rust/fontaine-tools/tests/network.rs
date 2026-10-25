//! The `[network]` settings against real sockets: a proxy that sees the request, and a local
//! rustls server that checks the handshake used the configured roots and client identity.

// The Windows platform verifier rejects the signature of the generated test certificates, so the
// handshake tests run off Windows and the helpers they alone use are unused there.
#![cfg_attr(windows, allow(dead_code, unused_imports))]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fontaine_tools::tls::{NetworkSettings, build_client};
use fontaine_tools::webfetch::{Webfetch, WebfetchArgs, WebfetchConfig};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};
use rig_core::tool::{Tool, ToolContext};
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Pki {
    ca_pem: String,
    ca_der: CertificateDer<'static>,
    issuer: Issuer<'static, KeyPair>,
}

fn pki() -> Pki {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = params.self_signed(&key).unwrap();
    Pki {
        ca_pem: ca.pem(),
        ca_der: ca.der().clone(),
        issuer: Issuer::new(params, key),
    }
}

struct Leaf {
    cert_pem: String,
    cert_der: CertificateDer<'static>,
    key_pem: String,
    key_der: Vec<u8>,
}

fn leaf(pki: &Pki, usage: ExtendedKeyUsagePurpose) -> Leaf {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::new()).unwrap();
    params.subject_alt_names = vec![SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST))];
    params.extended_key_usages = vec![usage];
    let cert = params.signed_by(&key, &pki.issuer).unwrap();
    Leaf {
        cert_pem: cert.pem(),
        cert_der: cert.der().clone(),
        key_pem: key.serialize_pem(),
        key_der: key.serialize_der(),
    }
}

/// A TLS server answering every request with `ok`; `Some(roots)` makes it demand a client
/// certificate chaining to them.
async fn tls_server(server: &Leaf, require_client: Option<&Pki>) -> SocketAddr {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap();
    let builder = match require_client {
        Some(pki) => {
            let mut roots = RootCertStore::empty();
            roots.add(pki.ca_der.clone()).unwrap();
            let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                .build()
                .unwrap();
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    };
    let config = builder
        .with_single_cert(
            vec![server.cert_der.clone()],
            PrivateKeyDer::try_from(server.key_der.clone()).unwrap(),
        )
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let mut seen = Vec::new();
                while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                    match tls.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => seen.extend_from_slice(&buf[..n]),
                    }
                }
                let _ = tls
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                    )
                    .await;
                let _ = tls.shutdown().await;
            });
        }
    });
    addr
}

fn write(dir: &tempfile::TempDir, name: &str, text: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn client(settings: NetworkSettings) -> reqwest::Client {
    let network = settings.load().unwrap();
    build_client(&network, reqwest::Client::builder).unwrap()
}

async fn get_text(client: &reqwest::Client, url: &str) -> Result<String, reqwest::Error> {
    client.get(url).send().await?.text().await
}

#[cfg(not(windows))]
#[tokio::test]
async fn a_client_without_the_ca_fails_the_handshake_and_one_with_it_succeeds() {
    let pki = pki();
    let server = leaf(&pki, ExtendedKeyUsagePurpose::ServerAuth);
    let addr = tls_server(&server, None).await;
    let url = format!("https://{addr}/");

    assert!(
        get_text(&client(NetworkSettings::default()), &url)
            .await
            .is_err()
    );

    let dir = tempfile::tempdir().unwrap();
    let trusting = client(NetworkSettings {
        ca_bundle: Some(write(&dir, "ca.pem", &pki.ca_pem)),
        ..Default::default()
    });
    assert_eq!(get_text(&trusting, &url).await.unwrap(), "ok");
}

#[cfg(not(windows))]
#[tokio::test]
async fn a_server_requiring_a_client_certificate_needs_the_identity() {
    let pki = pki();
    let server = leaf(&pki, ExtendedKeyUsagePurpose::ServerAuth);
    let addr = tls_server(&server, Some(&pki)).await;
    let url = format!("https://{addr}/");
    let dir = tempfile::tempdir().unwrap();
    let ca = write(&dir, "ca.pem", &pki.ca_pem);

    let anonymous = client(NetworkSettings {
        ca_bundle: Some(ca.clone()),
        ..Default::default()
    });
    assert!(get_text(&anonymous, &url).await.is_err());

    let me = leaf(&pki, ExtendedKeyUsagePurpose::ClientAuth);
    let identified = client(NetworkSettings {
        ca_bundle: Some(ca),
        client_cert: Some(write(&dir, "c.pem", &me.cert_pem)),
        client_key: Some(write(&dir, "k.pem", &me.key_pem)),
        ..Default::default()
    });
    assert_eq!(get_text(&identified, &url).await.unwrap(), "ok");
}

async fn proxy_mock(body: &str) -> MockServer {
    let proxy = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&proxy)
        .await;
    proxy
}

fn proxied(key: &str, proxy: &MockServer, no_proxy: Option<&str>) -> NetworkSettings {
    let mut settings = NetworkSettings {
        no_proxy: no_proxy.map(str::to_string),
        ..Default::default()
    };
    match key {
        "https" => settings.https_proxy = Some(proxy.uri()),
        _ => settings.http_proxy = Some(proxy.uri()),
    }
    settings
}

#[tokio::test]
async fn an_http_proxy_receives_plain_http_requests_in_absolute_form() {
    let proxy = proxy_mock("from the proxy").await;
    let client = client(proxied("http", &proxy, None));
    let body = get_text(&client, "http://origin.test/page").await.unwrap();
    assert_eq!(body, "from the proxy");
    let seen = proxy.received_requests().await.unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].url.host_str(), Some("origin.test"));
}

#[tokio::test]
async fn no_proxy_sends_the_listed_host_straight_to_its_origin() {
    let proxy = proxy_mock("from the proxy").await;
    let origin = proxy_mock("from the origin").await;
    let client = client(proxied("http", &proxy, Some("127.0.0.1")));
    let body = get_text(&client, &origin.uri()).await.unwrap();
    assert_eq!(body, "from the origin");
    assert!(proxy.received_requests().await.unwrap().is_empty());
}

#[cfg(not(windows))]
#[tokio::test]
async fn an_https_proxy_alone_leaves_http_direct_and_an_http_proxy_alone_leaves_https_direct() {
    let proxy = proxy_mock("from the proxy").await;
    let origin = proxy_mock("from the origin").await;
    let https_only = client(proxied("https", &proxy, None));
    assert_eq!(
        get_text(&https_only, &origin.uri()).await.unwrap(),
        "from the origin"
    );

    let pki = pki();
    let server = leaf(&pki, ExtendedKeyUsagePurpose::ServerAuth);
    let addr = tls_server(&server, None).await;
    let dir = tempfile::tempdir().unwrap();
    let http_only = client(NetworkSettings {
        ca_bundle: Some(write(&dir, "ca.pem", &pki.ca_pem)),
        ..proxied("http", &proxy, None)
    });
    assert_eq!(
        get_text(&http_only, &format!("https://{addr}/"))
            .await
            .unwrap(),
        "ok"
    );
    assert!(proxy.received_requests().await.unwrap().is_empty());
}

#[cfg(not(windows))]
#[tokio::test]
async fn an_https_request_tunnels_through_connect_with_the_proxy_credentials() {
    let pki = pki();
    let server = leaf(&pki, ExtendedKeyUsagePurpose::ServerAuth);
    let target = tls_server(&server, None).await;

    let heads = Arc::new(Mutex::new(Vec::<String>::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let recorded = heads.clone();
    tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        let mut seen = Vec::new();
        let mut buf = [0u8; 1024];
        while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = client.read(&mut buf).await.unwrap();
            if n == 0 {
                return;
            }
            seen.extend_from_slice(&buf[..n]);
        }
        recorded
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(&seen).into_owned());
        let mut upstream = tokio::net::TcpStream::connect(target).await.unwrap();
        client
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await
            .unwrap();
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    });

    let dir = tempfile::tempdir().unwrap();
    let client = client(NetworkSettings {
        https_proxy: Some(format!("http://user:pass@{proxy_addr}")),
        ca_bundle: Some(write(&dir, "ca.pem", &pki.ca_pem)),
        ..Default::default()
    });
    let body = get_text(&client, &format!("https://{target}/"))
        .await
        .unwrap();
    assert_eq!(body, "ok");
    let heads = heads.lock().unwrap();
    let head = heads[0].to_ascii_lowercase();
    assert!(head.starts_with(&format!("connect {target} ")), "{head}");
    // base64("user:pass")
    assert!(
        head.contains("proxy-authorization: basic dxnlcjpwyxnz"),
        "{head}"
    );
}

#[tokio::test]
async fn webfetch_goes_through_the_configured_proxy() {
    let proxy = proxy_mock("<p>from the proxy</p>").await;
    let network = proxied("http", &proxy, None).load().unwrap();
    let tool = Webfetch::new(WebfetchConfig::default(), &network).unwrap();
    let out = tool
        .call(
            &mut ToolContext::new(),
            WebfetchArgs {
                url: "http://origin.test/page".to_string(),
            },
        )
        .await
        .unwrap();
    assert!(out.contains("from the proxy"), "{out}");
    assert_eq!(proxy.received_requests().await.unwrap().len(), 1);
}

#[test]
fn a_bad_network_setting_never_yields_a_client() {
    let bad = NetworkSettings {
        ca_bundle: Some("/nonexistent/ca.pem".into()),
        ..Default::default()
    };
    assert!(bad.load().is_err());
}

#[test]
fn a_key_that_does_not_match_the_certificate_is_a_load_error_naming_both_files() {
    let pki = pki();
    let a = leaf(&pki, ExtendedKeyUsagePurpose::ClientAuth);
    let b = leaf(&pki, ExtendedKeyUsagePurpose::ClientAuth);
    let dir = tempfile::tempdir().unwrap();
    let settings = NetworkSettings {
        client_cert: Some(write(&dir, "cert.pem", &a.cert_pem)),
        client_key: Some(write(&dir, "key.pem", &b.key_pem)),
        ..Default::default()
    };
    let err = settings.load().unwrap_err();
    let text = fontaine_tools::error_chain::chain(&err);
    assert!(
        matches!(
            err,
            fontaine_tools::tls::NetworkError::IdentityMismatch { .. }
        ),
        "{text}"
    );
    assert!(text.contains("client_cert") && text.contains("cert.pem") && text.contains("key.pem"));
}

#[test]
fn a_matching_pair_loads() {
    let pki = pki();
    let a = leaf(&pki, ExtendedKeyUsagePurpose::ClientAuth);
    let dir = tempfile::tempdir().unwrap();
    let settings = NetworkSettings {
        client_cert: Some(write(&dir, "cert.pem", &a.cert_pem)),
        client_key: Some(write(&dir, "key.pem", &a.key_pem)),
        ..Default::default()
    };
    settings.load().unwrap();
}
