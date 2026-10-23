//! The device-authorization client against a `wiremock` stand-in for the authorization server.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fontaine_core::providers::device_flow::{
    DeviceCode, DeviceFlow, DeviceFlowEndpoints, FlowError, RefreshError,
};
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn flow(server: &MockServer) -> DeviceFlow {
    DeviceFlow::new(DeviceFlowEndpoints {
        code_url: format!("{}/auth/device/code", server.uri()),
        token_url: format!("{}/auth/device/token", server.uri()),
        client_id: "opencode-cli",
        code_params: vec![("supports_org_scope", json!(true))],
    })
    .unwrap()
    .with_request_timeout(None)
}

#[tokio::test]
async fn a_stalled_refresh_request_times_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/device/token"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;
    let err = flow(&server)
        .with_request_timeout(Some(Duration::from_millis(100)))
        .refresh("rt-1")
        .await
        .unwrap_err();
    assert!(matches!(err, RefreshError::Failed(FlowError::Request(_))));
}

fn code_reply(url: &str) -> serde_json::Value {
    json!({
        "device_code": "dc-1",
        "user_code": "ABCD-1234",
        "verification_uri_complete": url,
        "expires_in": 600,
        "interval": 5,
    })
}

async fn mount_code(server: &MockServer, reply: serde_json::Value) {
    Mock::given(method("POST"))
        .and(path("/auth/device/code"))
        .and(body_partial_json(
            json!({"client_id": "opencode-cli", "supports_org_scope": true}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply))
        .expect(1)
        .mount(server)
        .await;
}

fn token_error(error: &str) -> ResponseTemplate {
    ResponseTemplate::new(400).set_body_json(json!({ "error": error }))
}

fn granted() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "access_token": "at-1",
        "refresh_token": "rt-1",
        "expires_in": 3600,
        "org_id": "org_1",
    }))
}

fn a_code() -> DeviceCode {
    DeviceCode {
        device_code: "dc-1".to_string(),
        user_code: "ABCD-1234".to_string(),
        verification_url: "https://example.test/verify".to_string(),
        expires_in: Duration::from_secs(600),
        interval: Duration::from_secs(5),
    }
}

#[tokio::test]
async fn start_sends_the_client_id_and_scope_and_returns_the_code() {
    let server = MockServer::start().await;
    mount_code(
        &server,
        code_reply("https://example.test/verify?code=ABCD-1234"),
    )
    .await;
    let code = flow(&server).start().await.unwrap();
    assert_eq!(code.user_code, "ABCD-1234");
    assert_eq!(
        code.verification_url,
        "https://example.test/verify?code=ABCD-1234"
    );
    assert_eq!(code.interval, Duration::from_secs(5));
}

#[tokio::test]
async fn a_relative_verification_url_resolves_against_the_endpoint() {
    let server = MockServer::start().await;
    mount_code(
        &server,
        code_reply("/console/device?user_code=ABCD-1234&client_id=opencode-cli"),
    )
    .await;
    let code = flow(&server).start().await.unwrap();
    assert_eq!(
        code.verification_url,
        format!(
            "{}/console/device?user_code=ABCD-1234&client_id=opencode-cli",
            server.uri()
        )
    );
}

#[tokio::test]
async fn a_verification_url_that_is_not_http_is_refused() {
    for url in ["javascript:alert(1)", "file:///etc/passwd", "http://[::1"] {
        let server = MockServer::start().await;
        mount_code(&server, code_reply(url)).await;
        let err = flow(&server).start().await.unwrap_err();
        assert!(matches!(err, FlowError::Invalid(_)), "{url}: {err:?}");
    }
}

#[tokio::test]
async fn a_blank_verification_url_falls_back_or_is_refused() {
    let server = MockServer::start().await;
    let mut reply = code_reply("  ");
    reply["verification_uri"] = json!("https://example.com/device");
    mount_code(&server, reply).await;
    let code = flow(&server).start().await.unwrap();
    assert_eq!(code.verification_url, "https://example.com/device");

    for blank in ["", "   "] {
        let server = MockServer::start().await;
        mount_code(&server, code_reply(blank)).await;
        let err = flow(&server).start().await.unwrap_err();
        assert!(matches!(err, FlowError::Invalid(_)), "{blank:?}: {err:?}");
    }
}

#[tokio::test]
async fn polling_waits_through_pending_and_slows_down_by_five_seconds() {
    let server = MockServer::start().await;
    let answered = Arc::new(Mutex::new(0));
    let count = answered.clone();
    Mock::given(method("POST"))
        .and(path("/auth/device/token"))
        .and(body_partial_json(json!({
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            "device_code": "dc-1",
            "client_id": "opencode-cli",
        })))
        .respond_with(move |_: &wiremock::Request| {
            let mut count = count.lock().unwrap();
            *count += 1;
            match *count {
                1 => token_error("authorization_pending"),
                2 => token_error("slow_down"),
                _ => granted(),
            }
        })
        .expect(3)
        .mount(&server)
        .await;

    let waits = Arc::new(Mutex::new(Vec::new()));
    let record = waits.clone();
    let tokens = flow(&server)
        .poll_with(&a_code(), move |wait| {
            record.lock().unwrap().push(wait);
            std::future::ready(())
        })
        .await
        .unwrap();
    assert_eq!(
        *waits.lock().unwrap(),
        [
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_secs(10)
        ]
    );
    assert_eq!(
        (tokens.access.as_str(), tokens.refresh.as_str()),
        ("at-1", "rt-1")
    );
    assert_eq!(tokens.extra["org_id"], "org_1");
}

#[tokio::test(start_paused = true)]
async fn a_refusal_fails_naming_its_code() {
    for code in ["access_denied", "expired_token"] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/device/token"))
            .respond_with(token_error(code))
            .mount(&server)
            .await;
        let err = flow(&server).poll(&a_code()).await.unwrap_err();
        assert!(
            matches!(&err, FlowError::Rejected(c) if c == code),
            "{err:?}"
        );
        assert!(err.to_string().contains(code));
    }
}

#[tokio::test(start_paused = true)]
async fn a_code_nobody_approves_expires_locally() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/device/token"))
        .respond_with(token_error("authorization_pending"))
        .mount(&server)
        .await;
    let mut code = a_code();
    code.expires_in = Duration::from_secs(12);
    let err = flow(&server).poll(&code).await.unwrap_err();
    assert!(matches!(err, FlowError::Expired), "{err:?}");
    assert_eq!(err.to_string(), "the sign-in code expired");
}

#[tokio::test]
async fn refresh_sends_the_refresh_token_and_takes_the_rotated_pair() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/device/token"))
        .and(body_partial_json(json!({
            "grant_type": "refresh_token",
            "refresh_token": "rt-old",
            "client_id": "opencode-cli",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at-new",
            "refresh_token": "rt-new",
            "expires_in": 3600,
        })))
        .expect(1)
        .mount(&server)
        .await;
    let tokens = flow(&server).refresh("rt-old").await.unwrap();
    assert_eq!(
        (tokens.access.as_str(), tokens.refresh.as_str()),
        ("at-new", "rt-new")
    );
}

#[tokio::test]
async fn an_invalid_grant_means_signed_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/device/token"))
        .respond_with(token_error("invalid_grant"))
        .mount(&server)
        .await;
    let err = flow(&server).refresh("rt-old").await.unwrap_err();
    assert!(matches!(err, RefreshError::SignedOut), "{err:?}");
}
