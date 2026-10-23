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
    assert!(matches!(err, ConfigError::Read { .. }), "{err:?}");
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

mod writer {
    use super::*;
    use fontaine_core::credentials::{self, Target};

    fn file(w: &World) -> std::path::PathBuf {
        w.data.join("credentials.toml")
    }

    #[test]
    fn account_and_telemetry_logins_round_trip_through_the_lookup() {
        let w = World::new();
        credentials::store(&w.data, Target::Account("work"), "sk-work\n").unwrap();
        credentials::store(&w.data, Target::Telemetry, "ingest-key").unwrap();
        let c = Credentials::load(&w.data).unwrap();
        assert_eq!(c.account("work"), Some("sk-work"));
        assert_eq!(c.telemetry(), Some("ingest-key"));
    }

    #[test]
    fn the_file_is_owner_only() {
        let w = World::new();
        credentials::store(&w.data, Target::Account("work"), "sk-work").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(file(&w)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        #[cfg(windows)]
        common::assert_current_user_only(&file(&w));
    }

    #[test]
    fn a_data_dir_that_does_not_exist_yet_is_created() {
        let w = World::new();
        let nested = w.data.join("deeper").join("still");
        credentials::store(&nested, Target::Telemetry, "k").unwrap();
        assert_eq!(Credentials::load(&nested).unwrap().telemetry(), Some("k"));
    }

    #[test]
    fn a_later_login_keeps_the_earlier_entries() {
        let w = World::new();
        credentials::store(&w.data, Target::Account("work"), "k1").unwrap();
        credentials::store(&w.data, Target::Account("home"), "k2").unwrap();
        credentials::store(&w.data, Target::Telemetry, "k3").unwrap();
        let c = Credentials::load(&w.data).unwrap();
        assert_eq!(c.account("work"), Some("k1"));
        assert_eq!(c.account("home"), Some("k2"));
        assert_eq!(c.telemetry(), Some("k3"));

        credentials::store(&w.data, Target::Account("work"), "k1-new").unwrap();
        let c = Credentials::load(&w.data).unwrap();
        assert_eq!(c.account("work"), Some("k1-new"));
        assert_eq!(c.account("home"), Some("k2"));
        assert_eq!(c.telemetry(), Some("k3"));
    }

    #[test]
    fn an_empty_secret_is_a_usage_error_and_leaves_the_file_alone() {
        let w = World::new();
        credentials::store(&w.data, Target::Account("work"), "k1").unwrap();
        let before = std::fs::read_to_string(file(&w)).unwrap();
        for secret in ["", "\n", "\r\n"] {
            let err = credentials::store(&w.data, Target::Account("work"), secret).unwrap_err();
            assert!(matches!(err, ConfigError::Usage { .. }), "{err:?}");
            assert_eq!(err.exit_code(), 2);
        }
        assert_eq!(std::fs::read_to_string(file(&w)).unwrap(), before);

        let fresh = World::new();
        assert!(credentials::store(&fresh.data, Target::Telemetry, "").is_err());
        assert!(
            !file(&fresh).exists(),
            "no file is created for an empty secret"
        );
    }

    #[test]
    fn an_unparseable_existing_file_is_a_config_error_and_is_not_overwritten() {
        let w = World::new();
        let broken = "[accounts.work\napi_key = \"keep-me\"\n";
        write(&file(&w), broken);
        let err = credentials::store(&w.data, Target::Account("home"), "k").unwrap_err();
        assert!(matches!(err, ConfigError::Credentials { .. }), "{err:?}");
        assert_eq!(err.exit_code(), 2);
        assert_eq!(std::fs::read_to_string(file(&w)).unwrap(), broken);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_write_says_it_could_not_write() {
        use std::os::unix::fs::PermissionsExt;
        let w = World::new();
        std::fs::create_dir_all(&w.data).unwrap();
        std::fs::set_permissions(&w.data, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = credentials::store(&w.data, Target::Account("work"), "k");
        std::fs::set_permissions(&w.data, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Permission bits do not bind root.
        let Err(err) = result else { return };
        assert!(matches!(err, ConfigError::Write { .. }), "{err:?}");
        assert!(err.to_string().starts_with("cannot write "), "{err}");
    }
}
