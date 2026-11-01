//! The account operations behind the provider dialog, against a scratch config and data dir.

mod common;

use std::collections::HashMap;
use std::ffi::OsString;

use common::World;
use fontaine_core::accounts::{self, AccountsError, NewAccount, SignInState};
use fontaine_core::config::{AccountKind, ConfigError};
use fontaine_core::credentials::{self, Credentials, OAuthTokens, StoredCredential};

fn new_account(name: &str, kind: &str, base_url: Option<&str>) -> NewAccount {
    NewAccount {
        name: name.to_string(),
        kind: AccountKind::named(kind),
        base_url: base_url.map(str::to_string),
    }
}

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |name| map.get(name).map(OsString::from)
}

fn tokens(email: Option<&str>) -> OAuthTokens {
    OAuthTokens {
        access: "at".into(),
        refresh: "rt".into(),
        expires_at: 4_000_000_000,
        extra: email
            .map(|e| [("email".to_string(), e.to_string())].into())
            .unwrap_or_default(),
    }
}

#[test]
fn names_are_checked_against_the_dialog_rule_and_the_existing_accounts() {
    let w = World::new();
    w.user_file("[accounts.work]\nkind = \"anthropic\"\n");
    let config = w.load().unwrap();
    assert!(accounts::check_name("", &config).is_err());
    assert!(accounts::check_name("a/b", &config).is_err());
    assert!(accounts::check_name("a b", &config).is_err());
    assert!(accounts::check_name("é", &config).is_err());
    assert!(
        accounts::check_name("work", &config)
            .unwrap_err()
            .to_string()
            .contains("already exists")
    );
    for ok in ["home", "my-gateway_2", "X"] {
        accounts::check_name(ok, &config).unwrap();
    }
}

#[test]
fn build_runs_the_checks_the_loader_runs() {
    let w = World::new();
    let config = w.load().unwrap();
    let loader_error = {
        w.user_file("[accounts.gw]\nkind = \"openai-compatible\"\n");
        w.load().unwrap_err().to_string()
    };
    let built = accounts::build(&new_account("gw", "openai-compatible", None), &config);
    let message = built.unwrap_err().to_string();
    assert!(message.contains("accounts.gw.base_url"), "{message}");
    assert!(
        loader_error.contains("accounts.gw.base_url"),
        "{loader_error}"
    );
    assert!(message.contains("required"), "{message}");

    let account = accounts::build(
        &new_account(
            "gw",
            "openai-compatible",
            Some(" http://localhost:8080/v1 "),
        ),
        &config,
    )
    .unwrap();
    assert_eq!(
        account.base_url.as_deref(),
        Some("http://localhost:8080/v1")
    );
    assert_eq!(account.kind, AccountKind::named("openai-compatible"));
    assert!(account.api_key_env.is_none() && account.models.is_none());
}

#[test]
fn a_base_url_must_be_http_with_a_host() {
    let w = World::new();
    let config = w.load().unwrap();
    for bad in ["not a url", "ftp://host/x", "http://", "localhost:11434/x"] {
        assert!(
            accounts::build(&new_account("box", "ollama", Some(bad)), &config).is_err(),
            "{bad}"
        );
    }
    // Blank is the same as none for an optional URL.
    let account = accounts::build(&new_account("box", "ollama", Some("  ")), &config).unwrap();
    assert_eq!(account.base_url, None);
}

#[test]
fn add_writes_the_user_file_and_clears_a_credential_left_under_the_name() {
    let w = World::new();
    w.user_file("# mine\n[model]\nmodel = \"x/y\"\n");
    credentials::store(&w.data, "box", "sk-orphan").unwrap();
    credentials::store(&w.data, "other", "sk-other").unwrap();
    let config = w.load().unwrap();
    let user_file = w.config.join("config.toml");

    let account = accounts::add(
        &user_file,
        &w.data,
        &new_account("box", "ollama", Some("http://h:1")),
        &config,
    )
    .unwrap();
    assert_eq!(account.base_url.as_deref(), Some("http://h:1"));
    let text = std::fs::read_to_string(&user_file).unwrap();
    assert!(text.starts_with("# mine\n"), "{text}");
    assert!(text.contains("[accounts.box]\nkind = \"ollama\"\nbase_url = \"http://h:1\""));
    let stored = Credentials::load(&w.data).unwrap();
    assert!(stored.account("box").is_none(), "the orphan is gone");
    assert!(stored.account("other").is_some());
    assert!(w.load().unwrap().accounts.contains_key("box"));
}

#[test]
fn add_refuses_a_bad_account_without_writing() {
    let w = World::new();
    w.user_file("[accounts.work]\nkind = \"anthropic\"\n");
    let before = std::fs::read_to_string(w.config.join("config.toml")).unwrap();
    let config = w.load().unwrap();
    credentials::store(&w.data, "gw", "kept").unwrap();
    let err = accounts::add(
        &w.config.join("config.toml"),
        &w.data,
        &new_account("gw", "openai-compatible", None),
        &config,
    )
    .unwrap_err();
    assert!(matches!(err, AccountsError::Invalid(_)), "{err:?}");
    assert_eq!(
        std::fs::read_to_string(w.config.join("config.toml")).unwrap(),
        before
    );
    assert!(Credentials::load(&w.data).unwrap().account("gw").is_some());
}

#[test]
fn rows_say_what_each_account_authenticates_with() {
    let w = World::new();
    w.user_file(
        r#"
[accounts.stored]
kind = "anthropic"
[accounts.signed]
kind = "opencode-go"
[accounts.unsigned]
kind = "opencode-go"
base_url = "https://go.test"
[accounts.fromenv]
kind = "openai"
api_key_env = "SET_VAR"
[accounts.envunset]
kind = "openai"
api_key_env = "UNSET_VAR"
[accounts.keyless]
kind = "ollama"
[accounts.nokey]
kind = "gemini"
[accounts.switched]
kind = "anthropic"
"#,
    );
    credentials::store(&w.data, "stored", "sk").unwrap();
    credentials::store_oauth(&w.data, "signed", &tokens(Some("me@example.com"))).unwrap();
    // A sign-in left behind by a kind that has changed counts as nothing.
    credentials::store_oauth(&w.data, "switched", &tokens(None)).unwrap();
    let config = w.load().unwrap();
    let env = env_of(&[("SET_VAR", "v")]);
    let rows = accounts::rows(&config, &w.data, &env).unwrap();
    let state = |name: &str| rows.iter().find(|r| r.name == name).unwrap().state.clone();

    assert_eq!(state("stored"), SignInState::KeyStored);
    assert_eq!(
        state("signed"),
        SignInState::SignedIn {
            detail: Some("me@example.com".into())
        }
    );
    assert_eq!(state("unsigned"), SignInState::NotSignedIn);
    assert_eq!(
        state("fromenv"),
        SignInState::KeyFromEnv {
            var: "SET_VAR".into()
        }
    );
    assert_eq!(
        state("envunset"),
        SignInState::EnvUnset {
            var: "UNSET_VAR".into()
        }
    );
    assert_eq!(state("keyless"), SignInState::NoKeyNeeded);
    assert_eq!(state("nokey"), SignInState::NotSignedIn);
    assert_eq!(state("switched"), SignInState::NotSignedIn);

    let endpoint = |name: &str| {
        rows.iter()
            .find(|r| r.name == name)
            .unwrap()
            .endpoint
            .clone()
    };
    assert_eq!(endpoint("unsigned"), "https://go.test");
    assert_eq!(endpoint("signed"), "https://opencode.ai/inference/go");
    assert_eq!(endpoint("keyless"), "http://localhost:11434");
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
}

#[test]
fn rows_refuse_a_malformed_credentials_file() {
    let w = World::new();
    w.user_file("[accounts.a]\nkind = \"anthropic\"\n");
    std::fs::write(w.data.join("credentials.toml"), "[accounts.a\n").unwrap();
    let config = w.load().unwrap();
    let err = accounts::rows(&config, &w.data, &env_of(&[])).unwrap_err();
    assert!(matches!(err, ConfigError::Credentials { .. }), "{err:?}");
}

#[test]
fn log_out_removes_only_that_credential() {
    let w = World::new();
    accounts::store_key(&w.data, "a", "k1\n").unwrap();
    accounts::store_key(&w.data, "b", "k2").unwrap();
    assert!(accounts::log_out(&w.data, "a").unwrap());
    assert!(!accounts::log_out(&w.data, "a").unwrap());
    let stored = Credentials::load(&w.data).unwrap();
    assert!(stored.account("a").is_none());
    assert_eq!(
        stored.account("b"),
        Some(StoredCredential::ApiKey("k2".into()))
    );
}

#[test]
fn remove_deletes_the_account_and_its_credential() {
    let w = World::new();
    w.user_file("[accounts.a]\nkind = \"anthropic\"\n\n[accounts.b]\nkind = \"openai\"\n");
    accounts::store_key(&w.data, "a", "k1").unwrap();
    accounts::store_key(&w.data, "b", "k2").unwrap();
    accounts::remove(&w.config.join("config.toml"), &w.data, "a").unwrap();
    let config = w.load().unwrap();
    assert!(!config.accounts.contains_key("a") && config.accounts.contains_key("b"));
    let stored = Credentials::load(&w.data).unwrap();
    assert!(stored.account("a").is_none() && stored.account("b").is_some());
}

#[test]
fn remove_refuses_an_account_a_user_file_model_names() {
    for key in [
        "[model]\nmodel",
        "[guardian]\nmodel",
        "[guardian.composition]\nmodel",
        "[guardian.composition.reads.extra]\nmodel",
    ] {
        let w = World::new();
        w.user_file(&format!(
            "[accounts.a]\nkind = \"anthropic\"\n\n{key} = \"a/m\"\n"
        ));
        accounts::store_key(&w.data, "a", "k").unwrap();
        let file = w.config.join("config.toml");
        let err = accounts::remove(&file, &w.data, "a").unwrap_err();
        assert!(matches!(err, AccountsError::InUse { .. }), "{err:?}");
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .contains("[accounts.a]")
        );
        assert!(Credentials::load(&w.data).unwrap().account("a").is_some());
    }
}

#[test]
fn removing_an_account_from_another_file_changes_nothing() {
    let w = World::new();
    w.user_file("[model]\nmodel = \"p/m\"\n");
    let overlay = w.cli_file("[accounts.p]\nkind = \"anthropic\"\n");
    let config = w.load_with_settings(&overlay).unwrap();
    assert!(config.accounts.contains_key("p"));
    accounts::store_key(&w.data, "p", "k").unwrap();
    let err = accounts::remove(&w.config.join("config.toml"), &w.data, "p").unwrap_err();
    assert!(matches!(err, AccountsError::NotInUserFile), "{err:?}");
    assert!(Credentials::load(&w.data).unwrap().account("p").is_some());
}

#[tokio::test]
async fn a_kind_without_a_sign_in_refuses_one() {
    let w = World::new();
    w.user_file("[accounts.a]\nkind = \"anthropic\"\n");
    let config = w.load().unwrap();
    let err = accounts::sign_in(
        "a",
        &config.accounts["a"],
        &w.data,
        &Default::default(),
        &|_| {},
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("no sign-in"), "{err}");
}
