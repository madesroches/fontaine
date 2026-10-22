mod common;

use common::{World, write};
use fontaine_core::config::ConfigError;
use fontaine_core::credentials::Credentials;

const FILE: &str = r#"
[accounts.work]
api_key = "sk-work"

[accounts.local]
api_key = "sk-local"

[telemetry]
api_key = "ingest-key"
"#;

#[test]
fn account_and_telemetry_entries_round_trip() {
    let w = World::new();
    write(&w.data.join("credentials.toml"), FILE);
    let c = Credentials::load(&w.data).unwrap();
    assert_eq!(c.account("work"), Some("sk-work"));
    assert_eq!(c.account("local"), Some("sk-local"));
    assert_eq!(c.telemetry(), Some("ingest-key"));
}

#[test]
fn an_account_named_telemetry_does_not_shadow_the_telemetry_key() {
    let w = World::new();
    write(
        &w.data.join("credentials.toml"),
        "[accounts.telemetry]\napi_key = \"account-key\"\n\n[telemetry]\napi_key = \"ingest-key\"\n",
    );
    let c = Credentials::load(&w.data).unwrap();
    assert_eq!(c.account("telemetry"), Some("account-key"));
    assert_eq!(c.telemetry(), Some("ingest-key"));

    write(
        &w.data.join("credentials.toml"),
        "[accounts.telemetry]\napi_key = \"account-key\"\n",
    );
    let c = Credentials::load(&w.data).unwrap();
    assert_eq!(c.telemetry(), None);
}

#[test]
fn a_missing_file_or_entry_yields_no_credential() {
    let w = World::new();
    let c = Credentials::load(&w.data).unwrap();
    assert_eq!(c.account("work"), None);
    assert_eq!(c.telemetry(), None);

    write(&w.data.join("credentials.toml"), FILE);
    let c = Credentials::load(&w.data).unwrap();
    assert_eq!(c.account("nobody"), None);
}

#[test]
fn an_unparseable_file_is_a_config_error_naming_the_path() {
    let w = World::new();
    write(&w.data.join("credentials.toml"), "[accounts.x\n");
    let err = Credentials::load(&w.data).err().unwrap();
    assert!(matches!(err, ConfigError::Credentials { .. }), "{err:?}");
    assert!(err.to_string().contains("credentials.toml"));
    assert_eq!(err.exit_code(), 2);

    write(
        &w.data.join("credentials.toml"),
        "[accounts.x]\napi_key = \"k\"\nextra = 1\n",
    );
    assert!(matches!(
        Credentials::load(&w.data).err().unwrap(),
        ConfigError::Credentials { .. }
    ));
}

#[test]
fn a_non_not_found_io_error_is_a_config_error_naming_the_path() {
    let w = World::new();
    std::fs::create_dir_all(w.data.join("credentials.toml")).unwrap();
    let err = Credentials::load(&w.data).err().unwrap();
    assert!(matches!(err, ConfigError::Io { .. }), "{err:?}");
    assert!(err.to_string().contains("credentials.toml"));
    assert_eq!(err.exit_code(), 2);
}

#[test]
fn a_malformed_file_never_echoes_the_secret() {
    for body in [
        "[accounts.work]\napi_key = sk-abc123\n",
        "[accounts.work]\napikey = \"sk-abc123\"\n",
        "[accounts.work]\napi_key = \"sk-abc123\" sk-abc123\n",
        "[accounts.work]\napi_key = \"sk-abc123\"\nextra = sk-abc123\n",
        "telemetry = \"sk-abc123\"\n",
        "[accounts]\nwork = \"sk-abc123\"\n",
        "[accounts.sk-abc123]\napi_key = 1\n",
        "[telemetry]\nsk-abc123 = \"x\"\n",
        "[accounts.work]\napi_key = 12345\n",
        "[accounts.work]\napi_key = true\n",
    ] {
        let w = World::new();
        write(&w.data.join("credentials.toml"), body);
        let err = match Credentials::load(&w.data) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("{body}"),
        };
        assert!(!err.contains("sk-abc123"), "{err}");
        assert!(err.contains("credentials.toml"), "{err}");
        let err = fontaine_core::credentials::validate_credentials_toml(body).unwrap_err();
        assert!(!err.contains("sk-abc123"), "{err}");
    }
}
