//! The token source against a `wiremock` authorization server and a scratch data directory.

mod common;

use std::sync::Arc;

use common::World;
use fontaine_core::credentials::{self, Credentials, OAuthTokens, StoredCredential};
use fontaine_core::providers::device_flow::{DeviceFlow, DeviceFlowEndpoints, RefreshError};
use fontaine_core::providers::token_source::{Refresher, TokenSource, now_unix};
use futures::future::BoxFuture;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct AgainstServer(String);

impl Refresher for AgainstServer {
    fn refresh<'a>(
        &'a self,
        current: &'a OAuthTokens,
    ) -> BoxFuture<'a, Result<OAuthTokens, RefreshError>> {
        Box::pin(async move {
            DeviceFlow::new(DeviceFlowEndpoints {
                code_url: format!("{}/auth/device/code", self.0),
                token_url: format!("{}/auth/device/token", self.0),
                client_id: "opencode-cli",
                code_params: vec![],
            })
            .unwrap()
            .refresh(&current.refresh)
            .await
        })
    }
}

fn tokens(access: &str, refresh: &str, expires_at: u64) -> OAuthTokens {
    OAuthTokens {
        access: access.to_string(),
        refresh: refresh.to_string(),
        expires_at,
        extra: Default::default(),
    }
}

fn source(w: &World, server: &MockServer, held: OAuthTokens) -> Arc<TokenSource> {
    credentials::store_oauth(&w.data, "go", &held).unwrap();
    TokenSource::new(&w.data, "go", held, Arc::new(AgainstServer(server.uri())))
}

async fn rotating_server(expected_calls: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/device/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at-new",
            "refresh_token": "rt-new",
            "expires_in": 3600,
        })))
        .expect(expected_calls)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn a_fresh_token_is_served_without_a_request() {
    let w = World::new();
    let server = rotating_server(0).await;
    let source = source(&w, &server, tokens("at-1", "rt-1", now_unix() + 3600));
    assert_eq!(source.token().await.unwrap(), "at-1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_token_refreshes_once_for_ten_concurrent_callers_and_persists_the_rotation() {
    let w = World::new();
    let server = rotating_server(1).await;
    let source = source(&w, &server, tokens("at-1", "rt-1", now_unix() + 60));
    let calls = (0..10).map(|_| {
        let source = source.clone();
        tokio::spawn(async move { source.token().await.unwrap() })
    });
    for call in futures::future::join_all(calls).await {
        assert_eq!(call.unwrap(), "at-new");
    }
    let Some(StoredCredential::OAuth(stored)) = Credentials::load(&w.data).unwrap().account("go")
    else {
        panic!("expected an oauth entry");
    };
    assert_eq!(
        (stored.access.as_str(), stored.refresh.as_str()),
        ("at-new", "rt-new")
    );
}

#[tokio::test]
async fn a_pair_rotated_on_disk_by_another_process_is_adopted_without_a_request() {
    let w = World::new();
    let server = rotating_server(0).await;
    let source = source(&w, &server, tokens("at-1", "rt-1", now_unix() + 60));
    credentials::store_oauth(
        &w.data,
        "go",
        &tokens("at-other", "rt-other", now_unix() + 3600),
    )
    .unwrap();
    assert_eq!(source.token().await.unwrap(), "at-other");
}

#[tokio::test]
async fn a_dead_refresh_token_says_to_sign_in_again() {
    let w = World::new();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/device/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .mount(&server)
        .await;
    let source = source(
        &w,
        &server,
        tokens("at-1", "rt-1", now_unix().saturating_sub(10)),
    );
    let err = source.token().await.unwrap_err();
    assert!(err.to_string().contains("has ended"), "{err}");
    assert!(err.to_string().contains("/login go"), "{err}");
}

#[tokio::test]
async fn an_account_with_no_stored_sign_in_says_so() {
    let w = World::new();
    let server = MockServer::start().await;
    let source = TokenSource::new(
        &w.data,
        "go",
        tokens("at-1", "rt-1", 0),
        Arc::new(AgainstServer(server.uri())),
    );
    let err = source.token().await.unwrap_err();
    assert!(err.to_string().contains("/login go"), "{err}");
}

#[tokio::test]
async fn a_transient_refresh_failure_serves_a_token_that_has_not_expired() {
    let w = World::new();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/device/token"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let source = source(&w, &server, tokens("at-1", "rt-1", now_unix() + 60));
    assert_eq!(source.token().await.unwrap(), "at-1");

    let expired = TokenSource::new(
        &w.data,
        "go",
        tokens("at-1", "rt-1", now_unix().saturating_sub(10)),
        Arc::new(AgainstServer(server.uri())),
    );
    credentials::store_oauth(
        &w.data,
        "go",
        &tokens("at-1", "rt-1", now_unix().saturating_sub(10)),
    )
    .unwrap();
    assert!(expired.token().await.is_err());
}

struct CountingFailure(Arc<std::sync::atomic::AtomicU64>);

impl Refresher for CountingFailure {
    fn refresh<'a>(
        &'a self,
        _current: &'a OAuthTokens,
    ) -> BoxFuture<'a, Result<OAuthTokens, RefreshError>> {
        Box::pin(async move {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(RefreshError::Failed(
                fontaine_core::providers::device_flow::FlowError::Expired,
            ))
        })
    }
}

#[tokio::test]
async fn a_failed_refresh_is_not_retried_until_the_backoff_has_passed() {
    let w = World::new();
    let calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let held = tokens("at-1", "rt-1", now_unix() + 60);
    credentials::store_oauth(&w.data, "go", &held).unwrap();
    let source = TokenSource::new(
        &w.data,
        "go",
        held,
        Arc::new(CountingFailure(calls.clone())),
    );
    source.set_retry_backoff_secs(2);
    let seen = || calls.load(std::sync::atomic::Ordering::SeqCst);

    assert_eq!(source.token().await.unwrap(), "at-1");
    assert_eq!(seen(), 1);
    assert_eq!(source.token().await.unwrap(), "at-1");
    assert_eq!(seen(), 1, "served from the cache during the backoff");

    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    assert_eq!(source.token().await.unwrap(), "at-1");
    assert_eq!(seen(), 2, "retried after the backoff");
}

struct SlowRotation(Arc<tokio::sync::Notify>);

impl Refresher for SlowRotation {
    fn refresh<'a>(
        &'a self,
        _current: &'a OAuthTokens,
    ) -> BoxFuture<'a, Result<OAuthTokens, RefreshError>> {
        Box::pin(async move {
            self.0.notify_one();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            Ok(tokens("at-new", "rt-new", now_unix() + 3600))
        })
    }
}

#[tokio::test]
async fn a_dropped_caller_does_not_lose_the_rotated_pair() {
    let w = World::new();
    let held = tokens("at-old", "rt-old", now_unix() + 10);
    credentials::store_oauth(&w.data, "go", &held).unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let source = TokenSource::new(&w.data, "go", held, Arc::new(SlowRotation(started.clone())));

    let caller = tokio::spawn({
        let source = source.clone();
        async move { source.token().await }
    });
    started.notified().await;
    caller.abort();
    let _ = caller.await;

    assert_eq!(source.token().await.unwrap(), "at-new");
    let stored = Credentials::load(&w.data).unwrap();
    let Some(StoredCredential::OAuth(on_disk)) = stored.account("go") else {
        panic!("the account keeps its sign-in");
    };
    assert_eq!(on_disk.refresh, "rt-new");
}

#[cfg(unix)]
struct RotateThenLockDisk(std::path::PathBuf, std::sync::atomic::AtomicU32);

#[cfg(unix)]
impl Refresher for RotateThenLockDisk {
    fn refresh<'a>(
        &'a self,
        _current: &'a OAuthTokens,
    ) -> BoxFuture<'a, Result<OAuthTokens, RefreshError>> {
        use std::os::unix::fs::PermissionsExt;
        Box::pin(async move {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o555)).unwrap();
            Ok(tokens("at-new", "rt-new", now_unix() + 3600))
        })
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_rotated_pair_that_cannot_be_saved_is_still_served_and_kept() {
    use std::os::unix::fs::PermissionsExt;
    let w = World::new();
    let held = tokens("at-1", "rt-1", now_unix() + 60);
    credentials::store_oauth(&w.data, "go", &held).unwrap();
    let refresher = Arc::new(RotateThenLockDisk(w.data.clone(), Default::default()));
    let source = TokenSource::new(&w.data, "go", held, refresher.clone());

    let result = source.token().await;
    let again = source.token().await;
    std::fs::set_permissions(&w.data, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(result.unwrap(), "at-new");
    assert_eq!(again.unwrap(), "at-new");
    assert_eq!(refresher.1.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[cfg(unix)]
struct SeenRefreshTokens(std::path::PathBuf, std::sync::Mutex<Vec<String>>);

#[cfg(unix)]
impl Refresher for SeenRefreshTokens {
    fn refresh<'a>(
        &'a self,
        current: &'a OAuthTokens,
    ) -> BoxFuture<'a, Result<OAuthTokens, RefreshError>> {
        use std::os::unix::fs::PermissionsExt;
        Box::pin(async move {
            let mut seen = self.1.lock().unwrap();
            seen.push(current.refresh.clone());
            let n = seen.len();
            std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o555)).unwrap();
            Ok(tokens(
                &format!("at-{}", n + 1),
                &format!("rt-{}", n + 1),
                now_unix() + 60,
            ))
        })
    }
}

#[cfg(unix)]
#[tokio::test]
async fn the_next_refresh_after_a_failed_save_uses_the_rotated_token() {
    use std::os::unix::fs::PermissionsExt;
    let w = World::new();
    let held = tokens("at-1", "rt-1", now_unix() + 60);
    credentials::store_oauth(&w.data, "go", &held).unwrap();
    let refresher = Arc::new(SeenRefreshTokens(w.data.clone(), Default::default()));
    let source = TokenSource::new(&w.data, "go", held, refresher.clone());

    source.token().await.unwrap();
    source.token().await.unwrap();
    std::fs::set_permissions(&w.data, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(*refresher.1.lock().unwrap(), ["rt-1", "rt-2"]);
}
