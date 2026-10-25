//! The network settings reach the clients Fontaine builds for accounts.

mod common;

use common::World;
use fontaine_core::providers::{self, ConnectContext};
use fontaine_core::transport::reqwest_client;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn proxy() -> MockServer {
    let proxy = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"data":[]}"#))
        .mount(&proxy)
        .await;
    proxy
}

#[tokio::test]
async fn the_plain_client_goes_through_the_configured_proxy() {
    let proxy = proxy().await;
    let w = World::new();
    w.user_file(&format!("[network]\nhttp_proxy = \"{}\"\n", proxy.uri()));
    let network = w.load().unwrap().loaded_network(None).unwrap();
    let client = reqwest_client(&network).unwrap();
    client.get("http://origin.test/x").send().await.unwrap();
    assert_eq!(proxy.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn an_account_connects_with_its_own_proxy_over_the_global_one() {
    let global = proxy().await;
    let own = proxy().await;
    let w = World::new();
    w.user_file(&format!(
        "[network]\nhttp_proxy = \"{}\"\n\n[accounts.local]\nkind = \"ollama\"\nbase_url = \"http://ollama.test\"\nhttp_proxy = \"{}\"\n",
        global.uri(),
        own.uri()
    ));
    let config = w.load().unwrap();
    let account = &config.accounts["local"];
    let network = config.loaded_network(Some("local")).unwrap();
    let client = providers::find("ollama")
        .unwrap()
        .connect(ConnectContext {
            account_name: "local",
            account,
            credential: None,
            data_dir: &w.data,
            session_id: None,
            network: &network,
        })
        .unwrap();
    let _ = client.list().await;
    assert!(!own.received_requests().await.unwrap().is_empty());
    assert!(global.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_ca_bundle_that_is_not_valid_der_fails_the_client_build_with_its_cause() {
    let w = World::new();
    let bundle = w.data.join("bad-ca.pem");
    std::fs::write(
        &bundle,
        "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    w.user_file(&format!(
        "[network]\nca_bundle = \"{}\"\n",
        bundle.display().to_string().replace('\\', "/")
    ));
    let network = w.load().unwrap().loaded_network(None).unwrap();
    let error = reqwest_client(&network).expect_err("the certificate is not valid DER");
    assert_eq!(error.to_string(), "could not build the HTTP client");
    let text = fontaine_tools::error_chain::chain(&error);
    assert!(text.len() > error.to_string().len(), "{text}");
}
