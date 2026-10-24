mod common;

use common::{World, write};
use fontaine_core::credentials::{
    self, Credentials, OAuthTokens, StoredCredential, validate_credentials_toml,
};

const GO_FILE: &str = r#"
[accounts.go.oauth]
access = "at-1"
refresh = "rt-1"
expires_at = 1798000000
server = "https://opencode.ai/console"
org_id = "org_1"
email = "me@example.com"
"#;

fn tokens(access: &str, refresh: &str) -> OAuthTokens {
    OAuthTokens {
        access: access.to_string(),
        refresh: refresh.to_string(),
        expires_at: 1_798_000_000,
        extra: [("email".to_string(), "me@example.com".to_string())].into(),
    }
}

#[test]
fn an_oauth_entry_round_trips_with_its_metadata() {
    let w = World::new();
    write(&w.data.join("credentials.toml"), GO_FILE);
    let c = Credentials::load(&w.data).unwrap();
    let Some(StoredCredential::OAuth(t)) = c.account("go") else {
        panic!("expected an oauth entry");
    };
    assert_eq!((t.access.as_str(), t.refresh.as_str()), ("at-1", "rt-1"));
    assert_eq!(t.expires_at, 1_798_000_000);
    assert_eq!(t.extra["org_id"], "org_1");
    assert_eq!(t.extra["email"], "me@example.com");

    credentials::store_oauth(&w.data, "go", &tokens("at-2", "rt-2")).unwrap();
    let Some(StoredCredential::OAuth(t)) = Credentials::load(&w.data).unwrap().account("go") else {
        panic!("expected an oauth entry");
    };
    assert_eq!((t.access.as_str(), t.refresh.as_str()), ("at-2", "rt-2"));
    assert_eq!(t.extra["email"], "me@example.com");
}

#[test]
fn an_api_key_only_file_still_parses() {
    let w = World::new();
    write(
        &w.data.join("credentials.toml"),
        "[accounts.work]\napi_key = \"sk-work\"\n\n[telemetry]\napi_key = \"ingest\"\n",
    );
    let c = Credentials::load(&w.data).unwrap();
    assert_eq!(
        c.account("work"),
        Some(StoredCredential::ApiKey("sk-work".to_string()))
    );
    assert_eq!(c.telemetry(), Some("ingest"));
}

#[test]
fn an_entry_with_both_or_neither_is_malformed_and_never_echoes_values() {
    for body in [
        "[accounts.go]\napi_key = \"sk-abc123\"\n\n[accounts.go.oauth]\naccess = \"sk-abc123\"\nrefresh = \"r\"\nexpires_at = 1\n",
        "[accounts.go]\n",
        "[telemetry.oauth]\naccess = \"sk-abc123\"\nrefresh = \"r\"\nexpires_at = 1\n",
    ] {
        let w = World::new();
        write(&w.data.join("credentials.toml"), body);
        let err = match Credentials::load(&w.data) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("{body}"),
        };
        assert!(!err.contains("sk-abc123"), "{err}");
        let err = validate_credentials_toml(body).unwrap_err();
        assert!(!err.contains("sk-abc123"), "{err}");
        assert!(!err.contains("1:1"), "{err}");
    }
}

#[test]
fn the_documented_shape_validates() {
    validate_credentials_toml(GO_FILE).unwrap();
}

#[test]
fn the_stored_tokens_are_not_printed_by_debug() {
    let shown = format!(
        "{:?}",
        StoredCredential::OAuth(tokens("at-secret", "rt-secret"))
    );
    assert!(!shown.contains("secret"), "{shown}");
}

#[test]
fn an_api_key_store_waits_for_the_lock_and_keeps_a_rotation_made_meanwhile() {
    let w = World::new();
    credentials::store_oauth(&w.data, "go", &tokens("at-1", "rt-1")).unwrap();
    let held = credentials::lock(&w.data).unwrap();
    let data = w.data.clone();
    let writer = std::thread::spawn(move || {
        credentials::store(&data, "work", "sk-work").unwrap();
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!writer.is_finished(), "the store must wait for the lock");
    credentials::store_oauth_locked(&w.data, "go", &tokens("at-2", "rt-2"), &held).unwrap();
    drop(held);
    writer.join().unwrap();
    let c = Credentials::load(&w.data).unwrap();
    assert_eq!(
        c.account("work"),
        Some(StoredCredential::ApiKey("sk-work".to_string()))
    );
    let Some(StoredCredential::OAuth(t)) = c.account("go") else {
        panic!("expected an oauth entry");
    };
    assert_eq!(t.refresh, "rt-2");
}
